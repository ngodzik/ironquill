//! One question to the cheapest model: for what needs no strong model,
//! such as judging a command or explaining a few lines of code.

use ironquill_core::{
    ChatModel, ChatRequest, Effort, Message, ModelId, Pricing, TokenCount, Usage,
};

use crate::event::{Event, Purpose};

/// The usage prices are compared on: a question with some context, a
/// short answer.
const REFERENCE: Usage = Usage {
    input: TokenCount(10_000),
    output: TokenCount(1_000),
};

/// The cheapest of `candidates` with a known price, agents left out, and
/// its price. Another than `besides` when there is one: a model judging
/// its own command would judge little.
pub async fn cheapest<M: ChatModel>(
    model: &M,
    candidates: &[ModelId],
    besides: Option<&ModelId>,
) -> Option<(ModelId, Pricing)> {
    let mut cheapest: Option<(ModelId, Pricing)> = None;
    for candidate in candidates.iter().filter(|m| m.delegate().is_none()) {
        let Some(pricing) = model.pricing(candidate).await else {
            continue;
        };
        let other = |m: &ModelId| Some(m) != besides;
        let better = match &cheapest {
            None => true,
            Some((current, p)) => match (other(candidate), other(current)) {
                (true, false) => true,
                (false, true) => false,
                _ => pricing.cost(&REFERENCE).0 < p.cost(&REFERENCE).0,
            },
        };
        if better {
            cheapest = Some((candidate.clone(), pricing));
        }
    }
    cheapest
}

/// An answer of the cheapest model.
#[derive(Debug, Clone, PartialEq)]
pub struct Answered {
    /// What it said, trimmed.
    pub text: String,
    /// Who said it.
    pub model: ModelId,
    /// What it used.
    pub usage: Usage,
    /// What it cost, when the provider says.
    pub cost: Option<ironquill_core::Usd>,
}

/// Asks `to` one question, without tools, telling `observe` what the call
/// used. `None` when the model fails or says nothing.
pub async fn ask<M: ChatModel>(
    model: &M,
    to: &ModelId,
    system: &str,
    question: String,
    effort: Effort,
    mut observe: impl FnMut(Event),
) -> Option<Answered> {
    let request = ChatRequest {
        model: to.clone(),
        messages: vec![Message::system(system), Message::user(question)],
        tools: Vec::new(),
        effort: Some(effort),
    };
    let response = model.complete(&request).await.ok()?;
    observe(Event::Turn {
        model: to.clone(),
        usage: response.usage,
        cost: response.cost,
        subscription: false,
        context: None,
        cache: response.cache,
        purpose: Purpose::Other,
    });
    let text = response.content.unwrap_or_default().trim().to_owned();
    (!text.is_empty()).then(|| Answered {
        text,
        model: to.clone(),
        usage: response.usage,
        cost: response.cost,
    })
}

/// Asks the cheapest of `candidates` one question, as [`ask`] does.
/// `None` when none has a price, or as [`ask`].
pub async fn ask_cheapest<M: ChatModel>(
    model: &M,
    candidates: &[ModelId],
    system: &str,
    question: String,
    effort: Effort,
    observe: impl FnMut(Event),
) -> Option<Answered> {
    let (to, _) = cheapest(model, candidates, None).await?;
    ask(model, &to, system, question, effort, observe).await
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::Mutex;

    use ironquill_core::{ChatResponse, Usd};

    use super::*;

    /// Prices by model, and an answer naming who was asked.
    struct Priced(Vec<(&'static str, f64)>, Mutex<Vec<String>>);

    impl ChatModel for Priced {
        type Error = Infallible;

        async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, Infallible> {
            self.1.lock().unwrap().push(request.model.to_string());
            Ok(ChatResponse {
                content: Some(format!(" from {} ", request.model)),
                tool_calls: Vec::new(),
                usage: Usage {
                    input: TokenCount(50),
                    output: TokenCount(5),
                },
                cost: Some(Usd(0.0001)),
                cache: None,
            })
        }

        async fn pricing(&self, model: &ModelId) -> Option<Pricing> {
            let (_, price) = self.0.iter().find(|(m, _)| *m == model.as_str())?;
            Pricing::per_token(Usd(*price), Usd(*price * 4.0)).ok()
        }
    }

    #[tokio::test]
    async fn a_side_question_goes_to_the_cheapest_priced_model() {
        let model = Priced(
            vec![("strong", 1.5e-5), ("cheap", 1e-7), ("middle", 3e-6)],
            Mutex::new(Vec::new()),
        );
        let ids = |names: &[&str]| -> Vec<ModelId> {
            names.iter().map(|n| ModelId::new(*n).unwrap()).collect()
        };
        // An agent and a model without a price are passed over.
        let candidates = ids(&["claude-code/opus", "strong", "unpriced", "cheap", "middle"]);
        let mut events = Vec::new();
        let answered = ask_cheapest(
            &model,
            &candidates,
            "system",
            "what does this do?".into(),
            Effort::Low,
            |e| events.push(e),
        )
        .await
        .unwrap();
        assert_eq!(answered.model.as_str(), "cheap");
        assert_eq!(answered.text, "from cheap");
        assert_eq!(*model.1.lock().unwrap(), ["cheap"]);
        assert!(matches!(
            events[..],
            [Event::Turn {
                purpose: Purpose::Other,
                ..
            }]
        ));
        // Another than the one judged, when asked.
        let cheap = ModelId::new("cheap").unwrap();
        let (other, _) = cheapest(&model, &candidates, Some(&cheap)).await.unwrap();
        assert_eq!(other.as_str(), "middle");
        assert!(
            cheapest(&model, &ids(&["codex", "unpriced"]), None)
                .await
                .is_none()
        );
    }
}
