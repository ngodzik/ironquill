use std::fmt;

use ironquill_core::{
    ChatModel, ChatRequest, ChatResponse, Message, ModelId, Pricing, TokenCount, Usage, Usd,
};
use serde::{Deserialize, Serialize};

use crate::error::LlmError;

/// A provider that speaks the OpenAI compatible chat completions protocol.
///
/// `base_url` is the prefix both `/chat/completions` and `/models` hang off,
/// for example `https://router.requesty.ai/v1`.
#[derive(Clone)]
pub struct OpenAiCompatible {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl fmt::Debug for OpenAiCompatible {
    // Written by hand so that the key never reaches a log line or a panic message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatible")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl OpenAiCompatible {
    /// A provider at `base_url`, authenticated with `api_key` as a bearer token.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
        }
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

    async fn get(&self, url: &str) -> Result<String, LlmError> {
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
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
        let payload = WireRequest {
            model: request.model.as_str(),
            messages: &request.messages,
        };
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|source| transport(&url, source))?;
        let body = read_body(&url, response).await?;
        parse_completion(&url, &body)
    }
}

fn transport(url: &str, source: reqwest::Error) -> LlmError {
    LlmError::Transport {
        url: url.to_owned(),
        source,
    }
}

async fn read_body(url: &str, response: reqwest::Response) -> Result<String, LlmError> {
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

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
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
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
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
}

fn malformed(url: &str, reason: impl Into<String>) -> LlmError {
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
    let usage = wire.usage.map_or_else(Usage::default, |u| Usage {
        input: TokenCount(u.prompt_tokens),
        output: TokenCount(u.completion_tokens),
    });
    Ok(ChatResponse {
        content: choice.message.content.unwrap_or_default(),
        usage,
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
        assert_eq!(response.content, "hello");
        assert_eq!(response.usage.input, TokenCount(12));
        assert_eq!(response.usage.output, TokenCount(3));
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
    fn debug_never_shows_the_key() {
        let provider = OpenAiCompatible::new("https://example.test/v1", "rqsty-sk-secret");
        assert!(!format!("{provider:?}").contains("secret"));
    }
}
