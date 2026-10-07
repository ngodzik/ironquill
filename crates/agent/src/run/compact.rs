//! Compacting a conversation by subject: ironquill cuts it into exchanges,
//! the cheapest model groups them by subject from their titles, the person
//! picks what to keep, and the same model sums it up.

use ironquill_core::{
    Agent, ChatModel, Delegate, DelegateRequest, Effort, Message, ModelId, TokenCount,
};
use ironquill_tools::Toolbox;

use super::{
    Ctx, Judges, Ledger, SUMMARY_INPUT_BYTES, SUMMARY_PROMPT, Session, Summary, ask_cheapest,
    catch_up_all, latest, run_agent,
};
use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::event::Event;

/// Subjects of a conversation, for the person to pick what to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compaction {
    /// Each exchange's title: the start of its request.
    pub exchanges: Vec<String>,
    /// The subjects, each a name and its exchanges, every exchange in one.
    pub subjects: Vec<Subject>,
}

/// Exchanges about one thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    /// What it is about, in a few words.
    pub name: String,
    /// Indexes into [`Compaction::exchanges`].
    pub exchanges: Vec<usize>,
}

/// For the model grouping exchanges.
const GROUP_PROMPT: &str = "Group the exchanges of a conversation below by subject, from their \
titles. Reply with one line per subject, as `name: numbers`, such as `Parser tests: 1, 2, 5`. \
Each number in exactly one subject. Nothing else.";

/// The longest title of an exchange, in characters.
const TITLE_CHARS: usize = 90;

impl Session {
    /// The conversation's exchanges: each a request and what followed it,
    /// as ranges of its messages.
    fn exchange_ranges(&self) -> Vec<(usize, usize)> {
        let starts: Vec<usize> = (1..self.messages.len())
            .filter(|i| matches!(self.messages[*i], Message::User(_)))
            .collect();
        starts
            .iter()
            .enumerate()
            .map(|(n, start)| {
                (
                    *start,
                    starts.get(n + 1).copied().unwrap_or(self.messages.len()),
                )
            })
            .collect()
    }

    /// Cuts the conversation into exchanges and groups them by subject:
    /// the cheapest priced model of the team reads only their titles; with
    /// none, Claude Code's Haiku in a new session that only reads; failing
    /// that, each exchange is its own subject.
    ///
    /// # Errors
    ///
    /// [`AgentError::OverBudget`] when grouping would pass the budget.
    pub async fn plan_compaction<M: ChatModel, D: Delegate>(
        &mut self,
        model: &M,
        delegate: &D,
        toolbox: &mut Toolbox,
        config: &AgentConfig,
        observe: impl FnMut(Event) + Send,
    ) -> Result<Compaction, AgentError> {
        let exchanges: Vec<String> = self
            .exchange_ranges()
            .iter()
            .map(|(start, _)| match &self.messages[*start] {
                Message::User(text) => {
                    let first = text
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or_default();
                    let mut title: String = first.chars().take(TITLE_CHARS).collect();
                    if title.chars().count() < first.chars().count() {
                        title.push('…');
                    }
                    title
                }
                _ => String::new(),
            })
            .collect();
        let each = || {
            exchanges
                .iter()
                .enumerate()
                .map(|(i, title)| Subject {
                    name: title.clone(),
                    exchanges: vec![i],
                })
                .collect::<Vec<_>>()
        };
        if exchanges.len() < 2 {
            return Ok(Compaction {
                subjects: each(),
                exchanges,
            });
        }
        let list: String = exchanges
            .iter()
            .enumerate()
            .map(|(i, t)| format!("{}. {t}\n", i + 1))
            .collect();
        let mut ctx = Ctx::quiet(model, delegate, toolbox, config, observe);
        let reply = ask_small(
            &mut ctx,
            GROUP_PROMPT,
            list,
            "Grouping the conversation by subject",
        )
        .await;
        let subjects = reply
            .and_then(|reply| subjects_of(&reply, exchanges.len()))
            .unwrap_or_else(each);
        Ok(Compaction {
            exchanges,
            subjects,
        })
    }

    /// Compacts the conversation to a summary of the exchanges in `keep`,
    /// followed, when `last_as_is`, by the last exchange as it was. The
    /// agents' sessions end: their next request starts from the summary.
    /// Returns about how many tokens it held before, and after.
    ///
    /// # Errors
    ///
    /// [`AgentError::Config`] when no model can write the summary, and
    /// [`AgentError::OverBudget`] when it would pass the budget.
    #[allow(clippy::too_many_arguments)]
    pub async fn compact<M: ChatModel, D: Delegate>(
        &mut self,
        model: &M,
        delegate: &D,
        toolbox: &mut Toolbox,
        config: &AgentConfig,
        keep: &[usize],
        last_as_is: bool,
        observe: impl FnMut(Event) + Send,
    ) -> Result<(TokenCount, TokenCount), AgentError> {
        self.settle();
        let before = crate::context::approx_tokens(&self.messages);
        let ranges = self.exchange_ranges();
        let last = ranges.len().checked_sub(1).filter(|_| last_as_is);
        let kept: Vec<Message> = ranges
            .iter()
            .enumerate()
            .filter(|(i, _)| keep.contains(i) && Some(*i) != last)
            .flat_map(|(_, (start, end))| self.messages[*start..*end].iter().cloned())
            .collect();
        let mut compacted = vec![self.messages[0].clone()];
        if !kept.is_empty() {
            let text = latest(&catch_up_all(&kept), SUMMARY_INPUT_BYTES);
            let mut ctx = Ctx::quiet(model, delegate, toolbox, config, observe);
            let summary = ask_small(
                &mut ctx,
                SUMMARY_PROMPT,
                format!("The summary so far:\n(none yet)\n\nThe conversation since:\n{text}\n\nWrite the updated summary."),
                "Summing up what is kept",
            )
            .await
            .ok_or(AgentError::Config(
                "no model can write the summary: put a priced model in the team, or install Claude Code",
            ))?;
            compacted.push(Message::user(format!(
                "(The conversation so far, in short; what follows, if anything, is as it was.)\n{summary}"
            )));
            compacted.push(Message::Assistant {
                content: Some("Noted: I go on from this summary.".into()),
                tool_calls: Vec::new(),
            });
            self.summary = Some(Summary {
                text: summary,
                covers: compacted.len(),
            });
        } else {
            self.summary = None;
        }
        if let Some(last) = last {
            let (start, end) = ranges[last];
            compacted.extend_from_slice(&self.messages[start..end]);
        }
        self.messages = compacted;
        // Their sessions held what was just dropped.
        self.agents.clear();
        self.planners.clear();
        self.coders.clear();
        let after = crate::context::approx_tokens(&self.messages);
        Ok((TokenCount(before), TokenCount(after)))
    }
}

/// Asks the cheapest priced model one small question; with none, Claude
/// Code's Haiku in a new session that only reads.
async fn ask_small<M: ChatModel, D: Delegate, O: FnMut(Event) + Send>(
    ctx: &mut Ctx<'_, M, D, O>,
    system: &str,
    question: String,
    step: &str,
) -> Option<String> {
    if let Some((answer, _)) =
        ask_cheapest(ctx, None, system, question.clone(), Effort::Low, step).await
    {
        return Some(answer);
    }
    let haiku = ModelId::new("claude-code/haiku").ok()?;
    let request = DelegateRequest {
        agent: Agent::ClaudeCode,
        effort: Some(Effort::Low),
        model: "haiku".into(),
        prompt: format!("{system}\n\n{question}"),
        instructions: String::new(),
        resume: None,
        fork: false,
        ephemeral: true,
        directory: ctx.toolbox.workspace().root().to_owned(),
        read_only: true,
    };
    let reply = run_agent(ctx, &haiku, &request).await.ok()?;
    let text = reply.text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// The subjects a model named, when every exchange is in exactly one.
fn subjects_of(reply: &str, count: usize) -> Option<Vec<Subject>> {
    let mut seen = vec![false; count];
    let mut subjects = Vec::new();
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(['-', '*', ' ']);
        let Some((name, numbers)) = line.rsplit_once(':') else {
            continue;
        };
        let mut exchanges = Vec::new();
        for n in numbers.split([',', ' ']).filter(|n| !n.is_empty()) {
            let n: usize = n.trim().parse().ok()?;
            let i = n.checked_sub(1)?;
            if i >= count || seen[i] {
                return None;
            }
            seen[i] = true;
            exchanges.push(i);
        }
        if !exchanges.is_empty() {
            subjects.push(Subject {
                name: name.trim().trim_matches(['*', '`']).to_owned(),
                exchanges,
            });
        }
    }
    seen.iter().all(|s| *s).then_some(subjects)
}

impl<'a, M, D, O> Ctx<'a, M, D, O> {
    /// A context for work outside a request: no check, no thread.
    fn quiet(
        model: &'a M,
        delegate: &'a D,
        toolbox: &'a mut Toolbox,
        config: &'a AgentConfig,
        observe: O,
    ) -> Self {
        Ctx {
            model,
            delegate,
            config,
            toolbox,
            ledger: Ledger::new(),
            observe,
            thread: None,
            catch_up: String::new(),
            effort: config.effort,
            judges: Judges::Nothing,
            failing_before: Vec::new(),
            windows: Default::default(),
            allowed_secrets: Vec::new(),
            allowed_hosts: Vec::new(),
            last_call: None,
            last_context: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ironquill_core::{ChatRequest, ChatResponse, Pricing, Usage, Usd};
    use ironquill_tools::Workspace;

    use super::*;

    /// A priced model answering from a script.
    struct Script(Mutex<Vec<&'static str>>, Mutex<Vec<ChatRequest>>);

    impl ChatModel for Script {
        type Error = std::convert::Infallible;

        async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, Self::Error> {
            self.1.lock().unwrap().push(request.clone());
            Ok(ChatResponse {
                content: Some(self.0.lock().unwrap().remove(0).into()),
                tool_calls: vec![],
                usage: Usage::default(),
                cost: Some(Usd(0.0)),
                cache: None,
            })
        }

        async fn pricing(&self, _: &ModelId) -> Option<Pricing> {
            Pricing::per_token(Usd(1e-6), Usd(1e-6)).ok()
        }
    }

    struct NoAgent;

    impl Delegate for NoAgent {
        type Error = std::convert::Infallible;

        async fn run(
            &self,
            _: &DelegateRequest,
            _: &mut (dyn FnMut(ironquill_core::DelegateEvent) + Send),
        ) -> Result<ironquill_core::DelegateReply, Self::Error> {
            panic!("no agent here")
        }
    }

    #[tokio::test]
    async fn a_conversation_is_compacted_to_what_the_person_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let mut toolbox = Toolbox::new(Workspace::new(dir.path()).unwrap());
        let config = AgentConfig::builder()
            .tier(ModelId::new("cheap").unwrap())
            .build()
            .unwrap();
        let mut session = Session::new();
        for (asked, said) in [
            ("fix the parser", "Fixed."),
            ("add a test", "Added."),
            ("and the docs?", "Written."),
        ] {
            session.messages.push(Message::user(asked));
            session.messages.push(Message::Assistant {
                content: Some(said.into()),
                tool_calls: vec![],
            });
        }
        let model = Script(
            Mutex::new(vec![
                "Parser: 1, 2\nDocs: 3",
                "The parser was fixed and tested.",
            ]),
            Mutex::new(Vec::new()),
        );
        let plan = session
            .plan_compaction(&model, &NoAgent, &mut toolbox, &config, |_| {})
            .await
            .unwrap();
        assert_eq!(
            plan.exchanges,
            ["fix the parser", "add a test", "and the docs?"]
        );
        assert_eq!(plan.subjects[0].exchanges, [0, 1]);
        // Keep the parser's, the last as it was.
        session
            .compact(
                &model,
                &NoAgent,
                &mut toolbox,
                &config,
                &[0, 1, 2],
                true,
                |_| {},
            )
            .await
            .unwrap();
        assert!(
            matches!(&session.messages[1], Message::User(t) if t.ends_with("The parser was fixed and tested."))
        );
        assert!(matches!(&session.messages[3], Message::User(t) if t == "and the docs?"));
        assert_eq!(session.messages.len(), 5);
        // The summary was written from what was kept, the last left out.
        let asked = model.1.lock().unwrap();
        assert!(
            matches!(&asked[1].messages[1], Message::User(t) if t.contains("add a test") && !t.contains("and the docs?"))
        );
    }

    #[test]
    fn a_grouping_counts_only_when_each_exchange_is_in_one_subject() {
        let subjects = subjects_of("Parser: 1, 3\n- Tests: 2\n", 3).unwrap();
        assert_eq!(subjects[0].name, "Parser");
        assert_eq!(subjects[0].exchanges, [0, 2]);
        assert_eq!(subjects[1].exchanges, [1]);
        assert!(subjects_of("A: 1, 2", 3).is_none());
        assert!(subjects_of("A: 1, 2\nB: 2, 3", 3).is_none());
        assert!(subjects_of("A: 1, 4\nB: 2, 3", 3).is_none());
    }
}
