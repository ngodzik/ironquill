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
        // A real cost below a tenth of a cent would print as $0.000, which
        // reads as free. Saying it is under the threshold is the honest form.
        if self.0 > 0.0 && self.0 < 0.001 {
            f.write_str("<$0.001")
        } else {
            write!(f, "${:.3}", self.0)
        }
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

/// Who wrote a message in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Standing instructions for the model.
    System,
    /// The person, or the tool acting on their behalf.
    User,
    /// The model.
    Assistant,
}

/// One message in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Who wrote it.
    pub role: Role,
    /// What it says.
    pub content: String,
}

impl Message {
    /// A message from the user.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    /// A standing instruction for the model.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }
}

/// A request for one answer from one model.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    /// The model that should answer.
    pub model: ModelId,
    /// The conversation so far, oldest first.
    pub messages: Vec<Message>,
}

/// A model's answer and what it cost in tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    /// The text of the answer.
    pub content: String,
    /// The tokens the request consumed, as the provider reported them.
    pub usage: Usage,
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
    fn usd_display_never_rounds_a_cost_to_free() {
        assert_eq!(Usd(0.0).to_string(), "$0.000");
        assert_eq!(Usd(0.000_2).to_string(), "<$0.001");
        assert_eq!(Usd(0.041).to_string(), "$0.041");
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
