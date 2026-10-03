use std::fmt;
use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// The identifier a provider uses for a model, such as `openai/gpt-5-mini`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId(String);

impl ModelId {
    /// Builds a model identifier, refusing an empty one.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::EmptyModelId`] when `id` is empty or blank.
    pub fn new(id: impl Into<String>) -> Result<Self, CoreError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(CoreError::EmptyModelId);
        }
        Ok(Self(id))
    }

    /// The identifier as the provider expects it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `agent` with its own default model.
    pub fn agent(agent: Agent) -> Self {
        Self(agent.prefix().to_owned())
    }

    /// For a task handed to an agent rather than sent to a model: the agent,
    /// and the model it should use, empty for its own default.
    /// `claude-code/opus` gives Claude Code with `opus`, `codex` gives Codex
    /// with its default. Any other identifier is a model of the configured
    /// provider and gives `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ironquill_core::{Agent, ModelId};
    ///
    /// assert_eq!(
    ///     ModelId::new("claude-code/opus")?.delegate(),
    ///     Some((Agent::ClaudeCode, "opus"))
    /// );
    /// assert_eq!(ModelId::new("codex")?.delegate(), Some((Agent::Codex, "")));
    /// assert_eq!(ModelId::new("deepseek/deepseek-chat")?.delegate(), None);
    /// # Ok::<(), ironquill_core::CoreError>(())
    /// ```
    pub fn delegate(&self) -> Option<(Agent, &str)> {
        Agent::ALL.into_iter().find_map(|agent| {
            let prefix = agent.prefix();
            if self.0 == prefix {
                return Some((agent, ""));
            }
            let model = self.0.strip_prefix(prefix)?.strip_prefix('/')?;
            Some((agent, model))
        })
    }
}

/// A coding agent installed on the machine that ironquill can hand a task
/// to, through its command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Agent {
    /// Anthropic's Claude Code, the `claude` command.
    ClaudeCode,
    /// OpenAI's Codex, the `codex` command.
    Codex,
}

impl Agent {
    /// Every agent ironquill knows.
    pub const ALL: [Self; 2] = [Self::ClaudeCode, Self::Codex];

    /// The start of the model identifiers that hand a task to this agent.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        })
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A number of tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TokenCount(pub u64);

impl Add for TokenCount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for TokenCount {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl fmt::Display for TokenCount {
    /// Prints counts the way a status line wants them: `842`, `8.2k`, `1.3M`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Integer arithmetic keeps the rounding exact; going through f64 would
        // print 8.2k for 8249 on one platform and 8.3k on another in theory,
        // and a status line is not the place to wonder about that.
        let n = self.0;
        if n < 1_000 {
            write!(f, "{n}")
        } else if n < 1_000_000 {
            write!(f, "{}.{}k", n / 1_000, (n % 1_000) / 100)
        } else {
            write!(f, "{}.{}M", n / 1_000_000, (n % 1_000_000) / 100_000)
        }
    }
}

/// An amount in US dollars.
///
/// Prices per token are fractions of a millionth of a dollar, which an integer
/// count of cents or micro-dollars would round to zero. Amounts are only ever
/// summed and displayed, never compared for equality, so `f64` is enough.
#[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Usd(pub f64);

impl Add for Usd {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for Usd {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl fmt::Display for Usd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Cheap models cost fractions of a cent per request: below a cent four
        // decimals are shown, so that two requests can still be told apart.
        // Below what four decimals can show, saying so beats printing $0.0000,
        // which reads as free.
        // Zeros past the cents say nothing: $0.10, not $0.100.
        let text = match self.0 {
            0.0 => return f.write_str("$0.00"),
            x if x < 0.000_1 => return f.write_str("<$0.0001"),
            x if x < 0.01 => format!("{x:.4}"),
            x if x < 1.0 => format!("{x:.3}"),
            x => return write!(f, "${x:.2}"),
        };
        let (units, decimals) = text.split_once('.').unwrap_or((&text, ""));
        let decimals = decimals.trim_end_matches('0');
        write!(f, "${units}.{decimals:0<2}")
    }
}

/// What a model charges, per token, for what it reads and what it writes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    input_per_token: Usd,
    output_per_token: Usd,
}

impl Pricing {
    /// Builds a price list from per token amounts.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidPrice`] when either amount is negative,
    /// infinite or not a number.
    pub fn per_token(input: Usd, output: Usd) -> Result<Self, CoreError> {
        for price in [input, output] {
            if !price.0.is_finite() || price.0 < 0.0 {
                return Err(CoreError::InvalidPrice(price.0));
            }
        }
        Ok(Self {
            input_per_token: input,
            output_per_token: output,
        })
    }

    /// What `usage` costs at these prices.
    ///
    /// # Examples
    ///
    /// ```
    /// use ironquill_core::{Pricing, TokenCount, Usage, Usd};
    ///
    /// let pricing = Pricing::per_token(Usd(1e-6), Usd(4e-6))?;
    /// let usage = Usage { input: TokenCount(1_000), output: TokenCount(500) };
    /// assert!((pricing.cost(&usage).0 - 0.003).abs() < 1e-12);
    /// # Ok::<(), ironquill_core::CoreError>(())
    /// ```
    pub fn cost(&self, usage: &Usage) -> Usd {
        // Token counts stay far below 2^53, so the conversion is exact.
        Usd(usage.input.0 as f64 * self.input_per_token.0
            + usage.output.0 as f64 * self.output_per_token.0)
    }
}

/// The tokens one request consumed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens the model read: the prompt, the history, the context.
    pub input: TokenCount,
    /// Tokens the model wrote.
    pub output: TokenCount,
}

impl Add for Usage {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            input: self.input + rhs.input,
            output: self.output + rhs.output,
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

/// A tool the model may ask to call, described the way models expect.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// The name the model uses to call it.
    pub name: String,
    /// When and why to call it. Models choose tools from this text.
    pub description: String,
    /// A JSON Schema object describing the arguments.
    pub parameters: serde_json::Value,
}

/// One call the model asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's identifier for this call, echoed back with the result.
    pub id: String,
    /// Which tool.
    pub name: String,
    /// The arguments, as the JSON text the model wrote. Kept as text because a
    /// model can write invalid JSON, and that is the tool's error to report
    /// back, not a transport failure.
    pub arguments: String,
}

/// One message in a conversation.
///
/// The variants carry exactly what each kind of message can hold, so that a
/// tool result without a call id, say, cannot be built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    /// Standing instructions for the model.
    System(String),
    /// The person, or ironquill speaking on their behalf.
    User(String),
    /// What the model said, and the tools it asked to call.
    Assistant {
        /// Text, if the model wrote any alongside its calls.
        content: Option<String>,
        /// Calls to run before the conversation continues.
        tool_calls: Vec<ToolCall>,
    },
    /// The result of one tool call.
    Tool {
        /// The [`ToolCall::id`] this answers.
        call_id: String,
        /// What the tool returned, or why it failed.
        content: String,
    },
}

impl Message {
    /// A message from the user.
    pub fn user(content: impl Into<String>) -> Self {
        Self::User(content.into())
    }

    /// A standing instruction for the model.
    pub fn system(content: impl Into<String>) -> Self {
        Self::System(content.into())
    }
}

/// A request for one answer from one model.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    /// The model that should answer.
    pub model: ModelId,
    /// The conversation so far, oldest first.
    pub messages: Vec<Message>,
    /// The tools the model may call. Empty means none.
    pub tools: Vec<ToolSpec>,
}

/// How full a model's context was on a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUse {
    /// Tokens the model read on its last call: everything it was sent.
    pub used: TokenCount,
    /// The most the model can read at once.
    pub window: TokenCount,
}

impl ContextUse {
    /// The share of the window in use, in percent, rounded down.
    pub fn percent(&self) -> u64 {
        if self.window.0 == 0 {
            return 0;
        }
        self.used.0.saturating_mul(100) / self.window.0
    }
}

impl fmt::Display for ContextUse {
    /// `context 23% of 128k`, or `<1%` while barely used.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let window = match self.window.0 {
            w if w >= 1_000_000 && w % 1_000_000 == 0 => format!("{}M", w / 1_000_000),
            w if w >= 1_000 && w % 1_000 == 0 => format!("{}k", w / 1_000),
            _ => self.window.to_string(),
        };
        match self.percent() {
            0 if self.used.0 > 0 => write!(f, "context <1% of {window}"),
            p => write!(f, "context {p}% of {window}"),
        }
    }
}

/// A whole task for an external agent that reads and edits files itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateRequest {
    /// The agent to hand the task to.
    pub agent: Agent,
    /// The model the agent should use; empty for its own default.
    pub model: String,
    /// What to do.
    pub prompt: String,
    /// Standing instructions added to the agent's own.
    pub instructions: String,
    /// The agent's session to continue, to send it a follow-up such as a
    /// failing check; `None` starts a fresh one.
    pub resume: Option<String>,
    /// The project directory the agent works in.
    pub directory: std::path::PathBuf,
}

/// Something an external agent did, reported as it happens.
#[derive(Debug, Clone, PartialEq)]
pub enum DelegateEvent {
    /// A new block of text begins.
    TextStart,
    /// More text of the current block.
    Text(String),
    /// A tool call finished.
    Tool {
        /// The agent's name for the tool, such as `Read` or `Edit`.
        name: String,
        /// The arguments it was called with.
        input: serde_json::Value,
        /// What it returned, or the error it reported.
        output: Result<String, String>,
    },
}

/// How a delegated task ended.
#[derive(Debug, Clone, PartialEq)]
pub struct DelegateReply {
    /// The agent's closing message.
    pub text: String,
    /// The agent's session, to continue it with a follow-up.
    pub session: String,
    /// Tokens used, counting the context the agent read from its cache.
    pub usage: Usage,
    /// The agent's own estimate of what the tokens would cost at API prices.
    /// Not a bill: the agent may run on a subscription.
    pub estimate: Option<Usd>,
    /// How full its context was on its last call, when it said.
    pub context: Option<ContextUse>,
}

/// A model's answer and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    /// Text, if the model wrote any.
    pub content: Option<String>,
    /// Calls the model asked for. Empty means it considers itself done.
    pub tool_calls: Vec<ToolCall>,
    /// The tokens the request consumed, as the provider reported them.
    pub usage: Usage,
    /// The cost, when the provider reports it. Preferred over a price list
    /// lookup, since it accounts for caching and discounts the list cannot.
    pub cost: Option<Usd>,
}

impl ChatResponse {
    /// The answer as a conversation message, to append before the next turn.
    pub fn to_message(&self) -> Message {
        Message::Assistant {
            content: self.content.clone(),
            tool_calls: self.tool_calls.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn model_id_refuses_blank() {
        assert_eq!(ModelId::new("  "), Err(CoreError::EmptyModelId));
        assert!(ModelId::new("openai/gpt-5-mini").is_ok());
    }

    #[test]
    fn token_count_display() {
        assert_eq!(TokenCount(842).to_string(), "842");
        assert_eq!(TokenCount(8_249).to_string(), "8.2k");
        assert_eq!(TokenCount(1_400).to_string(), "1.4k");
        assert_eq!(TokenCount(1_350_000).to_string(), "1.3M");
    }

    #[test]
    fn context_use_reads_as_a_share_of_the_window() {
        let at = |used, window| ContextUse {
            used: TokenCount(used),
            window: TokenCount(window),
        };
        assert_eq!(at(32_000, 128_000).to_string(), "context 25% of 128k");
        assert_eq!(at(16_000, 1_000_000).to_string(), "context 1% of 1M");
        assert_eq!(at(900, 1_000_000).to_string(), "context <1% of 1M");
        assert_eq!(at(0, 0).percent(), 0);
    }

    #[test]
    fn usd_display_never_rounds_a_cost_to_free() {
        assert_eq!(Usd(0.10).to_string(), "$0.10");
        assert_eq!(Usd(0.127).to_string(), "$0.127");
        assert_eq!(Usd(0.005).to_string(), "$0.005");
        assert_eq!(Usd(0.0).to_string(), "$0.00");
        assert_eq!(Usd(0.000_02).to_string(), "<$0.0001");
        assert_eq!(Usd(0.000_23).to_string(), "$0.0002");
        assert_eq!(Usd(0.041).to_string(), "$0.041");
        assert_eq!(Usd(12.345).to_string(), "$12.35");
    }

    #[test]
    fn pricing_refuses_nonsense() {
        assert!(Pricing::per_token(Usd(-1.0), Usd(0.0)).is_err());
        assert!(Pricing::per_token(Usd(0.0), Usd(f64::NAN)).is_err());
        assert!(Pricing::per_token(Usd(f64::INFINITY), Usd(0.0)).is_err());
    }

    proptest! {
        /// The session total must not depend on how it was split into requests.
        #[test]
        fn cost_is_additive(
            a_in in 0u64..10_000_000, a_out in 0u64..10_000_000,
            b_in in 0u64..10_000_000, b_out in 0u64..10_000_000,
            p_in in 0.0f64..1e-4, p_out in 0.0f64..1e-4,
        ) {
            let pricing = Pricing::per_token(Usd(p_in), Usd(p_out)).unwrap();
            let a = Usage { input: TokenCount(a_in), output: TokenCount(a_out) };
            let b = Usage { input: TokenCount(b_in), output: TokenCount(b_out) };
            let split = pricing.cost(&a).0 + pricing.cost(&b).0;
            let whole = pricing.cost(&(a + b)).0;
            prop_assert!((split - whole).abs() <= 1e-9 * whole.max(1.0));
        }
    }
}
