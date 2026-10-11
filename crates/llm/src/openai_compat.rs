use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use ironquill_core::{
    CacheTtl, CacheUse, ChatModel, ChatRequest, ChatResponse, Message, ModelId, Pricing,
    TokenCount, ToolCall, ToolSpec, Usage, Usd,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::LlmError;

/// How many more times a request whose connection failed is tried.
const CONNECT_RETRIES: u32 = 2;

/// A provider that speaks the OpenAI compatible chat completions protocol.
///
/// `base_url` is the prefix both `/chat/completions` and `/models` hang off,
/// for example `https://router.requesty.ai/v1`.
#[derive(Clone)]
pub struct OpenAiCompatible {
    http: reqwest::Client,
    base_url: String,
    /// `None` when none was given: the interface still runs, for the agents
    /// that bring their own credentials, and a call to the provider fails
    /// saying why rather than reaching it unauthenticated.
    api_key: Option<String>,
    /// Context windows and prices by model, read once from the model list.
    known: Arc<Mutex<Option<HashMap<String, Known>>>>,
    /// How long what Claude's marks write stays in the cache.
    cache_ttl: CacheTtl,
}

impl fmt::Debug for OpenAiCompatible {
    // Written by hand so that the key never reaches a log line or a panic message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatible")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("cache_ttl", &self.cache_ttl)
            .finish()
    }
}

impl OpenAiCompatible {
    /// A provider at `base_url`, authenticated with `api_key` as a bearer token.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_key(base_url, Some(api_key.into()))
    }

    /// A provider at `base_url` with no key: every call to it fails with
    /// [`LlmError::NoKey`], before any request is sent.
    pub fn without_key(base_url: impl Into<String>) -> Self {
        Self::with_key(base_url, None)
    }

    fn with_key(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            known: Arc::new(Mutex::new(None)),
            cache_ttl: CacheTtl::default(),
        }
    }

    /// The same provider, keeping what Claude's cache marks write for
    /// `ttl`.
    #[must_use]
    pub fn with_cache_ttl(mut self, ttl: CacheTtl) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// What the provider is called where a model is shown with how it is
    /// reached: "Requesty", else its host.
    #[must_use]
    pub fn name(&self) -> String {
        if self.is_requesty() {
            return "Requesty".to_owned();
        }
        self.host().unwrap_or(&self.base_url).to_owned()
    }

    fn host(&self) -> Option<&str> {
        self.base_url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split(['/', ':']).next())
            .filter(|host| !host.is_empty())
    }

    /// The key, or why there is none to call `url` with.
    fn key(&self, url: &str) -> Result<&str, LlmError> {
        self.api_key.as_deref().ok_or_else(|| LlmError::NoKey {
            url: url.to_owned(),
        })
    }

    /// Whether the endpoint is Requesty, which takes options of its own.
    fn is_requesty(&self) -> bool {
        self.host()
            .is_some_and(|host| host == "requesty.ai" || host.ends_with(".requesty.ai"))
    }

    /// Looks up what `model` costs, from the provider's model list.
    ///
    /// Not every OpenAI compatible endpoint publishes prices; Requesty does, as
    /// `input_price` and `output_price` in dollars per token.
    ///
    /// # Errors
    ///
    /// [`LlmError::UnknownModel`] if the list does not contain `model`, or
    /// lists it without prices, and the transport errors of any request.
    pub async fn pricing(&self, model: &ModelId) -> Result<Pricing, LlmError> {
        let url = format!("{}/models", self.base_url);
        let body = self.get(&url).await?;
        parse_pricing(&url, &body, model)
    }

    /// Every model the provider lists, with its prices and context window
    /// when it gives them, to choose from.
    ///
    /// # Errors
    ///
    /// As [`Self::pricing`], when the list cannot be fetched or read.
    pub async fn list(&self) -> Result<Vec<Listed>, LlmError> {
        let url = format!("{}/models", self.base_url);
        let body = self.get(&url).await?;
        parse_list(&url, &body)
    }

    async fn get(&self, url: &str) -> Result<String, LlmError> {
        let key = self.key(url)?;
        let response = self
            .http
            .get(url)
            .bearer_auth(key)
            .send()
            .await
            .map_err(|source| transport(url, source))?;
        read_body(url, response).await
    }
}

impl ChatModel for OpenAiCompatible {
    type Error = LlmError;

    async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let url = format!("{}/chat/completions", self.base_url);
        let key = self.key(&url)?;
        let mut payload = wire_request(request);
        // Only to models the list does not say cannot reason: a provider may
        // refuse the field from one that cannot.
        if let Some(effort) = request.effort
            && self.known(&request.model).await.and_then(|k| k.reasoning) != Some(false)
        {
            payload["reasoning_effort"] = json!(effort.as_str());
        }
        if self.is_requesty() {
            // Each call of the agent resends the whole conversation, so most
            // of it can be a cache hit, billed at a fraction of the price.
            // Claude caches only what is marked, at its provider (Bedrock,
            // Vertex): Requesty's own marks go on the largest blocks and can
            // leave the end of a growing conversation out, so ironquill
            // places them. Other models are left to Requesty.
            let claude = is_claude(&request.model);
            if claude {
                mark_cache(&mut payload, self.cache_ttl);
            }
            payload["requesty"] = json!({"auto_cache": !claude});
        }
        // A connection that could not be made is tried again, twice, after a
        // pause: the request never left, so it cannot be billed twice. Any
        // other failure may have reached the provider and is reported.
        let mut tries = 0;
        let response = loop {
            match self
                .http
                .post(&url)
                .bearer_auth(key)
                .json(&payload)
                .send()
                .await
            {
                Ok(response) => break response,
                Err(e) if e.is_connect() && tries < CONNECT_RETRIES => {
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_secs(2 * u64::from(tries))).await;
                }
                Err(source) => return Err(transport(&url, source)),
            }
        };
        let body = read_body(&url, response).await?;
        parse_completion(&url, &body)
    }

    async fn context_window(&self, model: &ModelId) -> Option<u64> {
        self.known(model).await?.window
    }

    async fn pricing(&self, model: &ModelId) -> Option<Pricing> {
        self.known(model).await?.pricing
    }
}

/// What the model list says of one model.
#[derive(Debug, Clone, Copy)]
struct Known {
    window: Option<u64>,
    pricing: Option<Pricing>,
    reasoning: Option<bool>,
}

impl OpenAiCompatible {
    /// What the model list says of `model`, read once: a failure is
    /// remembered as "unknown" rather than asked again on every turn.
    async fn known(&self, model: &ModelId) -> Option<Known> {
        let cached = self.known.lock().ok()?.clone();
        let known = match cached {
            Some(known) => known,
            None => {
                let url = format!("{}/models", self.base_url);
                let known = match self.get(&url).await {
                    Ok(body) => parse_known(&body),
                    Err(_) => HashMap::new(),
                };
                if let Ok(mut cache) = self.known.lock() {
                    *cache = Some(known.clone());
                }
                known
            }
        };
        known.get(model.as_str()).copied()
    }
}

/// A model of the provider's list.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    /// Its identifier, as requests name it.
    pub id: String,
    /// Dollars per input token, when listed.
    pub input_price: Option<f64>,
    /// Dollars per output token, when listed.
    pub output_price: Option<f64>,
    /// The most it reads at once, in tokens, when listed.
    pub context_window: Option<u64>,
    /// What the provider says it is good at.
    pub description: Option<String>,
    /// Whether it can call tools, when listed.
    pub tool_calling: Option<bool>,
    /// Whether it reasons before answering, when listed.
    pub reasoning: Option<bool>,
    /// Whether it reads images, when listed.
    pub vision: Option<bool>,
    /// Its name without the host serving it, such as `glm-5.3-flash`.
    pub canonical: Option<String>,
}

fn parse_list(url: &str, body: &str) -> Result<Vec<Listed>, LlmError> {
    let wire: WireModelList =
        serde_json::from_str(body).map_err(|e| malformed(url, e.to_string()))?;
    Ok(wire
        .data
        .into_iter()
        .map(|m| Listed {
            id: m.id,
            input_price: m.input_price,
            output_price: m.output_price,
            context_window: m.context_window,
            description: m.description.filter(|d| !d.trim().is_empty()),
            tool_calling: m.supports_tool_calling,
            reasoning: m.supports_reasoning,
            vision: m.supports_vision,
            canonical: m.model_canonical_name,
        })
        .collect())
}

fn parse_known(body: &str) -> HashMap<String, Known> {
    serde_json::from_str::<WireModelList>(body)
        .map(|list| {
            list.data
                .into_iter()
                .map(|m| {
                    let pricing = match (m.input_price, m.output_price) {
                        (Some(input), Some(output)) => {
                            Pricing::per_token(Usd(input), Usd(output)).ok()
                        }
                        _ => None,
                    };
                    let known = Known {
                        window: m.context_window,
                        pricing,
                        reasoning: m.supports_reasoning,
                    };
                    (m.id, known)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn transport(url: &str, source: reqwest::Error) -> LlmError {
    LlmError::Transport {
        url: url.to_owned(),
        source,
    }
}

pub(crate) async fn read_body(url: &str, response: reqwest::Response) -> Result<String, LlmError> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|source| transport(url, source))?;
    if !status.is_success() {
        return Err(LlmError::Status {
            url: url.to_owned(),
            status: status.as_u16(),
            body,
        });
    }
    Ok(body)
}

/// Whether `model` is one of Anthropic's, whatever route serves it, such
/// as `bedrock/claude-opus-5-5`.
fn is_claude(model: &ModelId) -> bool {
    model.as_str().contains("claude")
}

/// Marks the system message and the last message with text as cached
/// until there, for a provider that caches only what is marked: the system
/// message with the tools before it, then the whole conversation, read back
/// on the next call. The text becomes a content block to carry the mark.
fn mark_cache(body: &mut Value, ttl: CacheTtl) {
    let mut mark = json!({"type": "ephemeral"});
    if ttl == CacheTtl::OneHour {
        mark["ttl"] = json!("1h");
    }
    let Some(messages) = body["messages"].as_array_mut() else {
        return;
    };
    let has_text = |m: &Value| m["content"].as_str().is_some_and(|t| !t.is_empty());
    let system = messages
        .iter()
        .position(|m| m["role"] == "system" && has_text(m));
    let last = messages.iter().rposition(has_text);
    for i in system.into_iter().chain(last) {
        let message = &mut messages[i];
        if let Some(text) = message["content"].as_str().map(str::to_owned) {
            message["content"] = json!([{"type": "text", "text": text, "cache_control": mark}]);
        }
    }
}

/// The request body. Built as JSON values rather than derived, because the
/// core message type is protocol neutral and this is the one place that knows
/// how OpenAI spells it.
fn wire_request(request: &ChatRequest) -> Value {
    let mut body = json!({
        "model": request.model.as_str(),
        "messages": request.messages.iter().map(wire_message).collect::<Vec<_>>(),
    });
    if !request.tools.is_empty() {
        body["tools"] = request.tools.iter().map(wire_tool).collect();
    }
    body
}

fn wire_message(message: &Message) -> Value {
    match message {
        Message::System(text) => json!({"role": "system", "content": text}),
        Message::User(text) => json!({"role": "user", "content": text}),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut m = json!({"role": "assistant", "content": content});
            if !tool_calls.is_empty() {
                m["tool_calls"] = tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": {"name": c.name, "arguments": c.arguments},
                        })
                    })
                    .collect();
            }
            m
        }
        Message::Tool { call_id, content } => {
            json!({"role": "tool", "tool_call_id": call_id, "content": content})
        }
    }
}

fn wire_tool(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        },
    })
}

#[derive(Deserialize)]
struct WireCompletion {
    choices: Vec<WireChoice>,
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessage,
}

#[derive(Deserialize)]
struct WireMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    id: String,
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    /// OpenAI's way of telling what came from the cache.
    #[serde(default)]
    prompt_tokens_details: Option<WirePromptDetails>,
    /// Anthropic's, when a gateway passes it through.
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    /// Requesty reports the cost of the call in dollars. Other endpoints
    /// leave it out.
    cost: Option<f64>,
}

#[derive(Deserialize)]
struct WirePromptDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
    /// What was written to the cache, as Requesty says it.
    #[serde(default)]
    caching_tokens: Option<u64>,
}

impl WireUsage {
    /// What the call read from the cache and wrote to it, when told.
    fn cache(&self) -> Option<CacheUse> {
        let read = self
            .cache_read_input_tokens
            .or_else(|| self.prompt_tokens_details.as_ref()?.cached_tokens);
        let written = self
            .cache_creation_input_tokens
            .or_else(|| self.prompt_tokens_details.as_ref()?.caching_tokens)
            .map(TokenCount);
        (read.is_some() || written.is_some()).then(|| CacheUse {
            read: TokenCount(read.unwrap_or(0)),
            written,
            rewrite_extra: None,
        })
    }
}

#[derive(Deserialize)]
struct WireModelList {
    data: Vec<WireModel>,
}

#[derive(Deserialize)]
struct WireModel {
    id: String,
    input_price: Option<f64>,
    output_price: Option<f64>,
    context_window: Option<u64>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    supports_tool_calling: Option<bool>,
    #[serde(default)]
    supports_reasoning: Option<bool>,
    #[serde(default)]
    supports_vision: Option<bool>,
    #[serde(default)]
    model_canonical_name: Option<String>,
}

pub(crate) fn malformed(url: &str, reason: impl Into<String>) -> LlmError {
    LlmError::Malformed {
        url: url.to_owned(),
        reason: reason.into(),
    }
}

fn parse_completion(url: &str, body: &str) -> Result<ChatResponse, LlmError> {
    let wire: WireCompletion =
        serde_json::from_str(body).map_err(|e| malformed(url, e.to_string()))?;
    let choice = wire
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| malformed(url, "no choices in the answer"))?;
    // A missing usage block is reported as zero rather than refused: the answer
    // is still worth showing, and the status line says the cost is unknown.
    let (usage, cost, cache) = wire.usage.map_or((Usage::default(), None, None), |u| {
        let usage = Usage {
            input: TokenCount(u.prompt_tokens),
            output: TokenCount(u.completion_tokens),
        };
        (
            usage,
            u.cost.filter(|c| c.is_finite() && *c >= 0.0).map(Usd),
            u.cache(),
        )
    });
    let tool_calls = choice
        .message
        .tool_calls
        .into_iter()
        .map(|c| ToolCall {
            id: c.id,
            name: c.function.name,
            arguments: c.function.arguments,
        })
        .collect();
    Ok(ChatResponse {
        content: choice.message.content.filter(|c| !c.is_empty()),
        tool_calls,
        usage,
        cost,
        cache,
    })
}

fn parse_pricing(url: &str, body: &str, model: &ModelId) -> Result<Pricing, LlmError> {
    let wire: WireModelList =
        serde_json::from_str(body).map_err(|e| malformed(url, e.to_string()))?;
    let entry = wire
        .data
        .into_iter()
        .find(|m| m.id == model.as_str())
        .ok_or_else(|| LlmError::UnknownModel(model.to_string()))?;
    let (Some(input), Some(output)) = (entry.input_price, entry.output_price) else {
        return Err(LlmError::UnknownModel(model.to_string()));
    };
    Pricing::per_token(Usd(input), Usd(output)).map_err(|e| malformed(url, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.test/v1/chat/completions";

    #[test]
    fn reads_answer_and_usage() {
        let body = r#"{
            "id": "x",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
        }"#;
        let response = parse_completion(URL, body).unwrap();
        assert_eq!(response.content.as_deref(), Some("hello"));
        assert_eq!(response.cost, None);
        assert_eq!(response.usage.input, TokenCount(12));
        assert_eq!(response.usage.output, TokenCount(3));
    }

    #[test]
    fn reads_tool_calls_and_reported_cost() {
        // Shape from the OpenAI chat completions spec: content is null and
        // arguments is a JSON string, not an object.
        let body = r#"{
            "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "read_file", "arguments": "{\"path\": \"a.rs\"}"}}
            ]}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 82, "completion_tokens": 18, "cost": 0.00042}
        }"#;
        let response = parse_completion(URL, body).unwrap();
        assert_eq!(response.content, None);
        assert_eq!(response.tool_calls[0].name, "read_file");
        assert_eq!(response.tool_calls[0].arguments, r#"{"path": "a.rs"}"#);
        assert_eq!(response.cost, Some(Usd(0.00042)));
    }

    #[tokio::test]
    async fn without_a_key_nothing_is_sent_and_the_error_says_what_to_do() {
        // Nothing listens on port 9: had a request been sent, the error would
        // be a transport one.
        let provider = OpenAiCompatible::without_key("http://127.0.0.1:9");
        let request = ChatRequest {
            model: ModelId::new("m").unwrap(),
            messages: vec![Message::user("go")],
            tools: vec![],
            effort: None,
        };
        let error = provider.complete(&request).await.unwrap_err();
        assert!(matches!(error, LlmError::NoKey { .. }), "{error:?}");
        assert!(error.to_string().contains("claude-code/"));
        assert!(matches!(provider.list().await, Err(LlmError::NoKey { .. })));
    }

    #[test]
    fn request_carries_tools_and_the_whole_tool_exchange() {
        let request = ChatRequest {
            model: ModelId::new("m").unwrap(),
            messages: vec![
                Message::user("go"),
                Message::Assistant {
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    }],
                },
                Message::Tool {
                    call_id: "call_1".into(),
                    content: "text".into(),
                },
            ],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
            effort: None,
        };
        let body = wire_request(&request);
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["messages"][1]["content"], Value::Null);
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["arguments"],
            "{}"
        );
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
    }

    #[test]
    fn caching_is_asked_of_requesty_only() {
        assert!(OpenAiCompatible::new("https://router.requesty.ai/v1", "k").is_requesty());
        assert!(!OpenAiCompatible::new("https://openrouter.ai/api/v1", "k").is_requesty());
        assert!(!OpenAiCompatible::new("https://requesty.ai.evil.test/v1", "k").is_requesty());
        assert!(!OpenAiCompatible::new("http://127.0.0.1:8080/v1", "k").is_requesty());
    }

    #[test]
    fn cache_reads_and_writes_as_requesty_reports_them() {
        // The shape of a Requesty answer for a Claude model on Bedrock.
        let body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "ok"}}],
            "usage": {"prompt_tokens": 14600, "completion_tokens": 12, "total_tokens": 14612,
                "prompt_tokens_details": {"cached_tokens": 9921, "caching_tokens": 4614,
                    "caching_token_details": {"caching_5m_tokens": 0, "caching_1h_tokens": 4614}},
                "cost": 0.0412}
        }"#;
        let cache = parse_completion(URL, body).unwrap().cache.unwrap();
        assert_eq!(cache.read, TokenCount(9921));
        assert_eq!(cache.written, Some(TokenCount(4614)));
        // Anthropic's own fields, when a gateway passes them, still win.
        let body = r#"{
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 1,
                "cache_read_input_tokens": 7, "cache_creation_input_tokens": 3}
        }"#;
        let cache = parse_completion(URL, body).unwrap().cache.unwrap();
        assert_eq!(
            (cache.read, cache.written),
            (TokenCount(7), Some(TokenCount(3)))
        );
    }

    #[test]
    fn claude_gets_marks_on_its_system_message_and_last_text() {
        let model = ModelId::new("bedrock/claude-opus-5-5").unwrap();
        assert!(is_claude(&model));
        assert!(!is_claude(&ModelId::new("deepseek/deepseek-chat").unwrap()));
        let request = ChatRequest {
            model,
            messages: vec![
                Message::System("rules".into()),
                Message::user("go"),
                Message::Assistant {
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    }],
                },
                Message::Tool {
                    call_id: "call_1".into(),
                    content: "text".into(),
                },
            ],
            tools: vec![],
            effort: None,
        };
        let mut body = wire_request(&request);
        mark_cache(&mut body, CacheTtl::FiveMinutes);
        let messages = &body["messages"];
        assert_eq!(messages[0]["content"][0]["text"], "rules");
        assert_eq!(
            messages[0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        // Only the system message and the last with text are marked.
        assert_eq!(messages[1]["content"], "go");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["content"][0]["text"], "text");
        assert!(messages[3]["content"][0]["cache_control"].is_object());

        let mut body = wire_request(&request);
        mark_cache(&mut body, CacheTtl::OneHour);
        assert_eq!(
            body["messages"][3]["content"][0]["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
    }

    #[test]
    fn a_provider_is_named_by_its_host() {
        assert_eq!(
            OpenAiCompatible::new("https://router.requesty.ai/v1", "k").name(),
            "Requesty"
        );
        assert_eq!(
            OpenAiCompatible::new("http://127.0.0.1:8080/v1", "k").name(),
            "127.0.0.1"
        );
    }

    #[test]
    fn missing_usage_is_zero_not_an_error() {
        let body = r#"{"choices": [{"message": {"content": "hi"}}]}"#;
        let response = parse_completion(URL, body).unwrap();
        assert_eq!(response.usage, Usage::default());
    }

    #[test]
    fn empty_choices_is_an_error() {
        let body = r#"{"choices": []}"#;
        assert!(matches!(
            parse_completion(URL, body),
            Err(LlmError::Malformed { .. })
        ));
    }

    #[test]
    fn finds_the_price_of_the_requested_model() {
        let body = r#"{"data": [
            {"id": "a/cheap", "input_price": 1e-7, "output_price": 4e-7},
            {"id": "b/free"}
        ]}"#;
        let model = ModelId::new("a/cheap").unwrap();
        let pricing = parse_pricing(URL, body, &model).unwrap();
        let usage = Usage {
            input: TokenCount(10_000),
            output: TokenCount(1_000),
        };
        assert!((pricing.cost(&usage).0 - 0.0014).abs() < 1e-12);

        let unpriced = ModelId::new("b/free").unwrap();
        assert!(matches!(
            parse_pricing(URL, body, &unpriced),
            Err(LlmError::UnknownModel(_))
        ));
    }

    #[test]
    fn the_model_list_reads_prices_and_windows() {
        let body = r#"{"data": [
            {"id": "a/big", "input_price": 1.4e-7, "output_price": 2.8e-7, "context_window": 1000000,
             "description": "Good at code.", "supports_tool_calling": false},
            {"id": "b/bare"}
        ]}"#;
        let list = parse_list(URL, body).unwrap();
        assert_eq!(list[0].id, "a/big");
        assert_eq!(list[0].input_price, Some(1.4e-7));
        assert_eq!(list[0].context_window, Some(1_000_000));
        assert_eq!(list[0].description.as_deref(), Some("Good at code."));
        assert_eq!(list[0].tool_calling, Some(false));
        assert_eq!(list[1].output_price, None);
        assert_eq!(list[1].tool_calling, None);
        assert!(parse_list(URL, "nope").is_err());
    }

    #[test]
    fn context_windows_come_from_the_model_list() {
        let body = r#"{"data": [
            {"id": "a/big", "context_window": 1000000},
            {"id": "b/unknown"}
        ]}"#;
        let known = parse_known(body);
        assert_eq!(known["a/big"].window, Some(1_000_000));
        assert_eq!(known["b/unknown"].window, None);
        assert!(parse_known("not json").is_empty());
    }

    #[test]
    fn debug_never_shows_the_key() {
        let provider = OpenAiCompatible::new("https://example.test/v1", "not-a-real-key");
        assert!(!format!("{provider:?}").contains("not-a-real-key"));
    }
}
