//! `/debug`: a question investigated in a fresh context. Every turn of a long
//! conversation resends all of it; an investigator starts from a brief and
//! the list of the exchanges instead, may read up to ten of them again, and
//! only its report comes back.
//!
//! The brief is written by the conversation's model as one more turn of its
//! own conversation, with the same system prompt, tools and effort, so that
//! the provider reads it all from its cache. An agent, or a model that
//! fails, leaves it to the cheapest model, from a flattened copy.

use serde_json::json;
use universe_core::{
    ChatModel, ChatRequest, Delegate, Effort, Message, ModelId, ToolCall, ToolSpec,
};
use universe_tools::Toolbox;

use super::{
    AgentConfig, AgentError, Ctx, Event, Judges, Ledger, Outcome, Purpose, Role, Session, Verdict,
    command_spec, converse, delegate_spec, flattened, identity, now_secs, within_budget,
};

/// How many times an investigator may look at the conversation again.
pub(super) const LOOKUPS: u32 = 10;

/// The most of one exchange `read_exchange` returns, in bytes.
const EXCHANGE_BYTES: usize = 12_000;

/// The conversation as the investigator may look at it.
pub(super) struct History {
    /// Each exchange as text, from a request to the next.
    exchanges: Vec<String>,
    /// How many looks it took.
    pub(super) lookups: u32,
}

const BRIEF_PROMPT: &str = "(Do not answer, and do not call any tool. Another model, which \
knows nothing of this conversation, will investigate the question below in a fresh context. \
Write its brief: what the question is about, what is known and was tried, the files, commands \
and errors that matter, and what is still unclear. Plain sentences, at most 300 words.)\n\nThe \
question: ";

const BRIEF_SYSTEM: &str = "You read a conversation between a person and a coding agent, and \
write the brief of an investigator who knows nothing of it: what the question is about, what is \
known and was tried, the files, commands and errors that matter, and what is still unclear. \
Plain sentences, at most 300 words.";

const INVESTIGATOR_PROMPT: &str = "You investigate one question about a software project, in a \
fresh context: read the code, run commands, and find the answer. You get a brief and the list \
of the conversation's exchanges; `search_history` and `read_exchange` look at them again, ten \
times at most, only when the brief is not enough. Change files only if the question asks for \
it. End with a report for the conversation: what you found, with the files and lines, and what \
to do. Only the report goes back.";

impl History {
    /// The exchanges of `messages`, from each request to the next.
    pub(super) fn of(messages: &[Message]) -> Self {
        let mut exchanges: Vec<String> = Vec::new();
        for message in messages.iter().skip(1) {
            if matches!(message, Message::User(_)) || exchanges.is_empty() {
                exchanges.push(String::new());
            }
            if let Some(last) = exchanges.last_mut() {
                last.push_str(&flattened(std::slice::from_ref(message)));
            }
        }
        Self {
            exchanges,
            lookups: 0,
        }
    }

    /// Each exchange in a line: its number, its request's start.
    fn index(&self) -> String {
        self.exchanges
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let start: String = text
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .chars()
                    .take(140)
                    .collect();
                format!("{}. {start}", i + 1)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The two tools that look at the conversation again.
    pub(super) fn specs() -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "search_history".into(),
                description: "Find the conversation's exchanges whose text holds every word \
                    given, with the lines that do. One of ten looks."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"],
                }),
            },
            ToolSpec {
                name: "read_exchange".into(),
                description: "Read one exchange of the conversation, by its number. One of \
                    ten looks."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"number": {"type": "integer"}},
                    "required": ["number"],
                }),
            },
        ]
    }

    /// Answers a call to one of its tools; `None` when the call is not one.
    pub(super) fn answer(&mut self, call: &ToolCall) -> Option<String> {
        if call.name != "search_history" && call.name != "read_exchange" {
            return None;
        }
        if self.lookups >= LOOKUPS {
            return Some(format!(
                "No look left: the {LOOKUPS} are used. Answer from what you have."
            ));
        }
        self.lookups += 1;
        let arguments: serde_json::Value =
            serde_json::from_str(&call.arguments).unwrap_or_default();
        Some(if call.name == "read_exchange" {
            let number = arguments["number"].as_u64().unwrap_or(0) as usize;
            match self.exchanges.get(number.wrapping_sub(1)) {
                Some(text) => text.chars().take(EXCHANGE_BYTES).collect(),
                None => format!("No exchange {number}: there are {}", self.exchanges.len()),
            }
        } else {
            let query = arguments["query"].as_str().unwrap_or("").to_lowercase();
            let words: Vec<&str> = query.split_whitespace().collect();
            let found: Vec<String> = self
                .exchanges
                .iter()
                .enumerate()
                .filter(|(_, text)| {
                    let lower = text.to_lowercase();
                    !words.is_empty() && words.iter().all(|w| lower.contains(w))
                })
                .map(|(i, text)| {
                    let lines: Vec<&str> = text
                        .lines()
                        .filter(|l| words.iter().any(|w| l.to_lowercase().contains(w)))
                        .take(3)
                        .collect();
                    format!("{}: {}", i + 1, lines.join(" / "))
                })
                .collect();
            if found.is_empty() {
                "Nothing found".to_owned()
            } else {
                found.join("\n")
            }
        })
    }
}

impl Session {
    /// Investigates `question` in a fresh context, with `investigator`, the
    /// model for fresh work; only its report is added to the conversation.
    ///
    /// # Errors
    ///
    /// As [`Session::send`]: the budget, a model that fails, a configuration
    /// without a model.
    #[allow(clippy::too_many_arguments)]
    pub async fn investigate<M: ChatModel, D: Delegate>(
        &mut self,
        model: &M,
        delegate: &D,
        toolbox: &mut Toolbox,
        config: &AgentConfig,
        investigator: &ModelId,
        question: &str,
        observe: impl FnMut(Event) + Send,
    ) -> Result<Outcome, AgentError> {
        self.settle();
        toolbox.reset_changes();
        let chat = config
            .tiers
            .first()
            .cloned()
            .ok_or(AgentError::Config("at least one model is needed"))?;
        let mut ctx = Ctx {
            model,
            delegate,
            config,
            toolbox,
            ledger: Ledger::new(),
            observe,
            purpose: Purpose::Chat,
            thread: None,
            catch_up: String::new(),
            effort: config.effort,
            judges: Judges::Nothing,
            failing_before: Vec::new(),
            windows: std::mem::take(&mut self.windows),
            allowed_secrets: config.allowed_secrets.clone(),
            allowed_hosts: config.allowed_hosts.clone(),
            allowed_programs: config.allowed_programs.clone(),
            context_warned: false,
            last_call: None,
            last_context: None,
            last_request: None,
            history: None,
        };
        let brief = self.brief(&mut ctx, &chat, question).await?;
        (ctx.observe)(Event::Delegating {
            from: chat.clone(),
            to: investigator.clone(),
            task: question.to_owned(),
        });
        let history = History::of(&self.messages);
        let index = history.index();
        ctx.history = Some(history);
        let mut messages = vec![
            Message::system(INVESTIGATOR_PROMPT),
            Message::user(format!(
                "The brief:\n{brief}\n\nThe conversation's exchanges:\n{index}\n\nThe question: \
                 {question}"
            )),
        ];
        let finished = converse(&mut ctx, investigator, &mut messages, Role::Member).await?;
        let report = messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::Assistant {
                    content: Some(text),
                    ..
                } if !text.trim().is_empty() => Some(text.trim().to_owned()),
                _ => None,
            })
            .unwrap_or_else(|| "(no report)".to_owned());
        let turns = messages
            .iter()
            .filter(|m| matches!(m, Message::Assistant { .. }))
            .count();
        let lookups = ctx.history.as_ref().map_or(0, |h| h.lookups);
        let changed: Vec<String> = ctx.toolbox.changed().map(str::to_owned).collect();
        let footer = format!(
            "{investigator} investigated in a fresh context: {turns} turn{}, {lookups} look{} at \
             the conversation, {}{}",
            if turns == 1 { "" } else { "s" },
            if lookups == 1 { "" } else { "s" },
            if changed.is_empty() {
                "no file changed".to_owned()
            } else {
                format!("files changed: {}", changed.join(", "))
            },
            if finished {
                ""
            } else {
                "; it ran out of turns"
            }
        );
        let report = format!("{report}\n\n({footer})");
        // Only the question and the report join the conversation; what the
        // investigator read stays out.
        self.asked.insert(self.messages.len(), now_secs());
        self.messages.push(Message::user(format!(
            "(An investigator looked into this in a fresh context: {question})"
        )));
        self.messages.push(Message::Assistant {
            content: Some(report.clone()),
            tool_calls: Vec::new(),
        });
        (ctx.observe)(Event::Investigated { text: report });
        self.windows = std::mem::take(&mut ctx.windows);
        Ok(ctx.ledger.outcome(Verdict::Answered, ctx.toolbox))
    }

    /// The investigator's brief: written by the conversation's model as one
    /// more turn of its own, read from its cache; else by the cheapest model
    /// from a flattened copy.
    async fn brief<M: ChatModel, D, O: FnMut(Event) + Send>(
        &self,
        ctx: &mut Ctx<'_, M, D, O>,
        chat: &ModelId,
        question: &str,
    ) -> Result<String, AgentError> {
        if chat.delegate().is_none() {
            // The same prefix the conversation sends: system prompt, tools.
            let mut tools = ctx.toolbox.specs();
            tools.push(command_spec());
            if let Some(spec) = delegate_spec(&ctx.config.team, chat) {
                tools.push(spec);
            }
            if let Some(external) = &ctx.config.external {
                tools.extend(external.specs());
            }
            let mut sent = self.messages.clone();
            if let Some(Message::System(prompt)) = sent.first_mut() {
                prompt.push_str(&identity(chat, true, ctx.config));
            }
            sent.push(Message::user(format!("{BRIEF_PROMPT}{question}")));
            let request = ChatRequest {
                model: chat.clone(),
                messages: sent,
                tools,
                effort: ctx.effort,
            };
            let pricing = ctx.model.pricing(chat).await;
            within_budget(ctx, pricing, &request)?;
            if let Ok(response) = ctx.model.complete(&request).await {
                ctx.ledger.usage += response.usage;
                match response.cost {
                    Some(cost) => ctx.ledger.cost += cost,
                    None => ctx.ledger.cost_complete = false,
                }
                (ctx.observe)(Event::Turn {
                    model: chat.clone(),
                    usage: response.usage,
                    cost: response.cost,
                    subscription: false,
                    context: None,
                    cache: response.cache,
                    purpose: Purpose::Other,
                });
                if let Some(text) = response.content.filter(|t| !t.trim().is_empty())
                    && response.tool_calls.is_empty()
                {
                    return Ok(text.trim().to_owned());
                }
            }
        }
        let flat = flattened(&self.messages[1.min(self.messages.len())..]);
        let flat: String = flat
            .chars()
            .rev()
            .take(60_000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let written = super::ask_cheapest(
            ctx,
            None,
            BRIEF_SYSTEM,
            format!("The conversation:\n{flat}\n\nThe question: {question}"),
            Effort::Low,
            "Writing the investigator's brief",
        )
        .await;
        Ok(written.map_or_else(
            || format!("(No brief could be written.) The question: {question}"),
            |(text, _)| text,
        ))
    }
}
