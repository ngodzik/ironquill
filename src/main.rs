//! The `ironquill` command.

#![deny(unsafe_code)]

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use ironquill_agent::{AgentConfig, Event, Member, Outcome, Verdict};
use ironquill_core::{ChatModel, ChatRequest, Effort, Message, ModelId, Usd};
use ironquill_llm::{
    Agents, ArtificialAnalysis, ClaudeCode, Codex, Listed, OpenAiCompatible, RANKINGS_SOURCE,
    Ranking, find_ranking,
};
use ironquill_tools::{Check, ToolSummary, Toolbox, Workspace};
use ironquill_tui::Defaults;
use tracing_subscriber::EnvFilter;

/// How many tracked file names go to the model up front. Enough to orient it
/// in a typical project, bounded so that a monorepo does not fill the context.
const FILE_LIST_LIMIT: usize = 300;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Base URL of an OpenAI compatible endpoint.
    #[arg(
        long,
        env = "IRONQUILL_BASE_URL",
        default_value = "https://router.requesty.ai/v1",
        global = true
    )]
    base_url: String,

    /// API key for that endpoint. Read from the environment so that it never
    /// lands in the shell history.
    #[arg(long, env = "IRONQUILL_API_KEY", hide_env_values = true, global = true)]
    api_key: Option<String>,

    /// Model tried first in the interface. Can be set later with `:model`.
    /// Typed here, it wins over the one kept for new sessions; from
    /// IRONQUILL_MODEL, only when none was kept.
    #[arg(long, env = "IRONQUILL_MODEL")]
    model: Option<String>,

    /// Stronger models for the interface, cheapest first.
    #[arg(long = "escalate", value_name = "MODEL")]
    escalate: Vec<String>,

    /// Models offered by the model picker (Ctrl-E), comma separated. When
    /// the claude command is installed, `claude-code/opus` and
    /// `claude-code/sonnet` are offered too; when the codex command is,
    /// `codex/<model>` for each model Codex lists.
    #[arg(long, env = "IRONQUILL_MODELS", value_delimiter = ',')]
    models: Vec<String>,

    /// Checks for the interface. Defaults as for `do`.
    #[arg(long = "check", value_name = "COMMAND")]
    checks: Vec<String>,

    /// How hard models think before answering: low, medium, high, xhigh
    /// or max. Defaults to the one kept for new sessions, or high.
    #[arg(long, env = "IRONQUILL_EFFORT", global = true)]
    effort: Option<Effort>,

    /// The most one request may cost, in dollars. Past it the work stops
    /// and the model says where it is. Defaults to the one kept with
    /// /defaults, or 0.10.
    #[arg(long, env = "IRONQUILL_BUDGET")]
    budget: Option<f64>,

    /// Continue this project's most recent conversation.
    #[arg(short = 'c', long = "continue", conflicts_with = "resume")]
    continue_last: bool,

    /// Continue a saved conversation of this project: pick it, or give its
    /// id or the start of it.
    #[arg(short = 'r', long, num_args = 0..=1, value_name = "ID")]
    resume: Option<Option<String>>,

    /// Without a subcommand, ironquill opens its terminal interface.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Ask one question and print the answer with what it cost.
    Ask {
        /// The question.
        prompt: String,

        /// The model that answers, as the provider names it.
        #[arg(long, env = "IRONQUILL_MODEL")]
        model: String,
    },

    /// Change the project in the current directory until its checks pass.
    Do {
        /// What to do, in plain words.
        task: String,

        /// The model tried first. Pick a cheap one: it is only kept if the
        /// checks pass.
        #[arg(long, env = "IRONQUILL_MODEL")]
        model: String,

        /// A stronger model to call if the previous one cannot make the checks
        /// pass. Repeat to add more, cheapest first.
        #[arg(long = "escalate", value_name = "MODEL")]
        escalate: Vec<String>,

        /// A command that must succeed, run without a shell. Repeat to add
        /// more. Defaults to `cargo check --all-targets` then `cargo test` in a
        /// Rust project.
        #[arg(long = "check", value_name = "COMMAND")]
        checks: Vec<String>,

        /// How many times one model may try before the next takes over.
        #[arg(long, default_value_t = 2)]
        rounds: u32,

        /// How many turns one try may take.
        #[arg(long, default_value_t = 30)]
        max_turns: u32,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Cli::command().get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    // A choice typed on the command line wins over the one kept for new
    // sessions; one from the environment does not, so that a variable set
    // long ago does not undo what was picked since.
    let typed = |id: &str| matches.value_source(id) == Some(ValueSource::CommandLine);
    let api_key = cli.api_key.context("no API key: set IRONQUILL_API_KEY")?;
    let provider = OpenAiCompatible::new(cli.base_url, api_key);

    let Some(command) = cli.command else {
        // No log subscriber here: anything written to the terminal while the
        // interface owns it would tear the screen.
        let start = if cli.continue_last {
            ironquill_tui::Start::Continue
        } else if let Some(resume) = cli.resume {
            match resume {
                Some(id) => ironquill_tui::Start::Id(id),
                None => ironquill_tui::Start::Pick,
            }
        } else {
            ironquill_tui::Start::New
        };
        let choices = Choices {
            model: Pick::new(cli.model, typed("model")),
            escalate: cli.escalate,
            offered: cli.models,
            checks: cli.checks,
            budget: Pick::new(cli.budget, typed("budget")),
            effort: Pick::new(cli.effort, typed("effort")),
        };
        return interface(provider, choices, start).await;
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    match command {
        Command::Ask { prompt, model } => {
            ask(&provider, &prompt, &model, cli.effort.unwrap_or_default()).await
        }
        Command::Do {
            task,
            model,
            escalate,
            checks,
            rounds,
            max_turns,
        } => {
            let mut builder = AgentConfig::builder()
                .tier(ModelId::new(model)?)
                .rounds_per_tier(rounds)
                .max_turns(max_turns);
            for model in escalate {
                builder = builder.tier(ModelId::new(model)?);
            }
            let detect = checks.is_empty();
            for check in checks_or_default(checks)? {
                builder = builder.check(check);
            }
            builder = builder
                .detect_checks(detect)
                .instructions(Defaults::instructions())
                .project_rules(ironquill_tools::project_instructions(Path::new(".")));
            builder = builder.effort(Some(cli.effort.unwrap_or_default()));
            let kept = Defaults::path()
                .and_then(|path| Defaults::load(&path).ok())
                .unwrap_or_default();
            let config = builder
                .build()?
                .with_audit_log(Defaults::audit_log_path())
                .with_allowed_secrets(kept.allowed_secrets)
                .with_allowed_hosts(kept.allowed_hosts)
                .with_strict_commands(!kept.lenient_commands);
            run_task(&provider, &config, &task).await
        }
    }
}

/// What the command line chose for the interface; the rest comes from the
/// defaults kept with /defaults.
struct Choices {
    model: Pick<String>,
    escalate: Vec<String>,
    offered: Vec<String>,
    checks: Vec<String>,
    budget: Pick<f64>,
    effort: Pick<Effort>,
}

/// A choice given at startup, and whether it was typed or came from the
/// environment.
struct Pick<T> {
    typed: Option<T>,
    from_env: Option<T>,
}

impl<T> Pick<T> {
    fn new(value: Option<T>, typed: bool) -> Self {
        if typed {
            Self {
                typed: value,
                from_env: None,
            }
        } else {
            Self {
                typed: None,
                from_env: value,
            }
        }
    }

    /// What was typed, else what was `kept` for new sessions, else what
    /// the environment says.
    fn or_kept(self, kept: Option<T>) -> Option<T> {
        self.typed.or(kept).or(self.from_env)
    }
}

async fn interface(
    provider: OpenAiCompatible,
    choices: Choices,
    start: ironquill_tui::Start,
) -> Result<()> {
    let workspace = Workspace::new(".")?;
    let defaults = match Defaults::path() {
        Some(path) => Defaults::load(&path).map_err(anyhow::Error::msg)?,
        None => Defaults::default(),
    };

    // A model typed at the command line replaces the one kept: say so, a
    // command recalled from the history would hide it otherwise.
    let mut notes = Vec::new();
    if let (Some(typed), Some(kept)) = (&choices.model.typed, &defaults.model)
        && typed != kept
    {
        notes.push(format!(
            "Model {typed}, given with --model, in place of your choice kept for new sessions, \
             {kept}. Start without --model to use it"
        ));
    }
    let mut tiers = Vec::new();
    for id in choices
        .model
        .or_kept(defaults.model)
        .into_iter()
        .chain(choices.escalate)
    {
        tiers.push(ModelId::new(id)?);
    }
    let detect_checks = choices.checks.is_empty();
    let checks = checks_or_default(choices.checks)?;

    let claude = ClaudeCode::find();
    let codex = Codex::find();
    // Prices for the agents' calls, fetched only when one is installed.
    let prices = if claude.is_some() || codex.is_some() {
        prices().await
    } else {
        None
    };
    let claude = claude.map(|c| c.with_prices(prices.clone()));
    let codex = codex.map(|c| {
        let billed = c.uses_api_key();
        c.with_prices(prices.clone()).billed(billed)
    });
    let mut models: Vec<ModelId> = Vec::new();
    for id in choices.offered.iter().chain(&defaults.models) {
        let id = ModelId::new(id.trim())?;
        if !models.contains(&id) {
            models.push(id);
        }
    }
    if claude.is_some() {
        for id in ["claude-code/opus", "claude-code/sonnet"] {
            let id = ModelId::new(id)?;
            if !models.contains(&id) {
                models.push(id);
            }
        }
    }
    if let Some(codex) = &codex {
        for id in codex.models() {
            let id = ModelId::new(format!("codex/{id}"))?;
            if !models.contains(&id) {
                models.push(id);
            }
        }
    }
    let team = defaults
        .team
        .iter()
        .map(ModelId::new)
        .collect::<Result<Vec<_>, _>>()?;
    let budget = choices
        .budget
        .or_kept(defaults.budget)
        .unwrap_or(Defaults::BUDGET);
    // The provider's list, to search and to price the team; without it the
    // interface still works, with less to show.
    let listed = tokio::time::timeout(Duration::from_secs(10), provider.list())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let chosen: Vec<&ModelId> = models.iter().chain(&team).collect();
    let rankings = rankings(&listed, &chosen).await;
    let catalog = listed
        .into_iter()
        .filter_map(|listed| {
            let ranking = find_ranking(&rankings, &listed.id, listed.canonical.as_deref());
            Some(Member {
                note: note(&listed, ranking),
                about: about(&listed, ranking),
                tools: listed.tool_calling != Some(false),
                score: ranking.and_then(|r| r.intelligence),
                price: listed.input_price,
                model: ModelId::new(listed.id).ok()?,
            })
        })
        .collect();

    let settings = ironquill_tui::Settings {
        tiers,
        checks,
        rounds: 2,
        max_turns: 30,
        models,
        team,
        budget: (budget > 0.0).then_some(Usd(budget)),
        effort: {
            let kept = match defaults.effort.as_deref() {
                Some(kept) => Some(kept.parse().map_err(anyhow::Error::msg)?),
                None => None,
            };
            choices.effort.or_kept(kept).unwrap_or_default()
        },
        planner: defaults.planner.as_deref().map(ModelId::new).transpose()?,
        usage_window: defaults
            .usage_window
            .as_deref()
            .and_then(ironquill_tui::parse_window),
        allowed_secrets: defaults.allowed_secrets.clone(),
        strict_commands: !defaults.lenient_commands,
        allowed_hosts: defaults.allowed_hosts.clone(),
        // With Claude Code installed, a pair is Opus planning, Sonnet coding.
        pair_mode: defaults.pair_mode.unwrap_or(claude.is_some()),
        tick: defaults.tick,
        detect_checks,
        notes,
        catalog,
        credits: (!rankings.is_empty()).then(|| RANKINGS_SOURCE.to_owned()),
    };
    // Without an agent installed, choosing one of its models fails with a
    // message saying so rather than at startup.
    let agents = Agents {
        claude: claude.unwrap_or_else(|| ClaudeCode::new("claude")),
        codex: codex.unwrap_or_else(|| Codex::new("codex")),
    };
    ironquill_tui::run(
        Arc::new(provider),
        Arc::new(agents),
        workspace,
        settings,
        start,
    )
    .await?;
    Ok(())
}

/// List prices for Claude Code's messages, from LiteLLM's list kept a week
/// next to the defaults, as ccusage prices them.
async fn prices() -> Option<Arc<ironquill_llm::PriceTable>> {
    let kept = Defaults::path()?.with_file_name("prices.json");
    ironquill_llm::load_prices(&kept).await.map(Arc::new)
}

/// The scores kept from Artificial Analysis, and when they were fetched.
#[derive(serde::Serialize, serde::Deserialize)]
struct Scores {
    fetched: u64,
    rankings: Vec<Ranking>,
}

/// Scores for the models, from the copy kept on disk. Scores of a model do
/// not change once measured: the list is fetched again only when it is a
/// month old, or a day old and missing a model picked here. Without
/// `ARTIFICIAL_ANALYSIS_API_KEY` only the kept copy is used.
async fn rankings(listed: &[Listed], chosen: &[&ModelId]) -> Vec<Ranking> {
    const DAY: u64 = 24 * 60 * 60;
    let Some(path) = Defaults::path().map(|p| p.with_file_name("rankings.json")) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let kept: Option<Scores> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let age = kept.as_ref().map(|k| now.saturating_sub(k.fetched));
    let missing = |rankings: &[Ranking]| {
        chosen
            .iter()
            .filter(|m| m.delegate().is_none())
            .any(|model| {
                let canonical = listed
                    .iter()
                    .find(|l| l.id == model.as_str())
                    .and_then(|l| l.canonical.as_deref());
                find_ranking(rankings, model.as_str(), canonical).is_none()
            })
    };
    let stale = match (&kept, age) {
        (Some(kept), Some(age)) => age > 30 * DAY || (age > DAY && missing(&kept.rankings)),
        _ => true,
    };
    let key = std::env::var("ARTIFICIAL_ANALYSIS_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty());
    if stale && let Some(key) = key {
        let fetched = tokio::time::timeout(
            Duration::from_secs(10),
            ArtificialAnalysis::new(key.trim()).rankings(),
        )
        .await;
        if let Ok(Ok(rankings)) = fetched {
            let scores = Scores {
                fetched: now,
                rankings,
            };
            if let Ok(json) = serde_json::to_vec(&scores) {
                let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
                let _ = std::fs::write(&path, json);
            }
            return scores.rankings;
        }
    }
    kept.map(|k| k.rankings).unwrap_or_default()
}

/// A score out of 100, as a whole number.
fn score(name: &str, value: Option<f64>) -> Option<String> {
    value.map(|v| format!("{name} {v:.0}"))
}

/// A few words on a listed model for the person and the model choosing:
/// `code 38 · intel 41 · $0.14 / $0.28 per M tokens · 1M context`.
fn note(listed: &Listed, ranking: Option<&Ranking>) -> String {
    let per_million = |price: f64| {
        let dollars = price * 1_000_000.0;
        let text = format!("{dollars:.3}");
        let text = text.trim_end_matches('0');
        let text = if text.ends_with('.') {
            format!("{text}00")
        } else if text.split('.').nth(1).is_some_and(|d| d.len() == 1) {
            format!("{text}0")
        } else {
            text.to_owned()
        };
        format!("${text}")
    };
    let price = match (listed.input_price, listed.output_price) {
        (Some(input), Some(output)) => Some(format!(
            "{} / {} per M tokens",
            per_million(input),
            per_million(output)
        )),
        _ => None,
    };
    let window = listed.context_window.map(|w| match w {
        w if w >= 1_000_000 && w % 1_000_000 == 0 => format!("{}M context", w / 1_000_000),
        w if w >= 1_000 => format!("{}k context", w / 1_000),
        w => format!("{w} context"),
    });
    let scores = ranking
        .into_iter()
        .flat_map(|r| [score("code", r.coding), score("intel", r.intelligence)])
        .flatten();
    scores
        .chain(price)
        .chain(window)
        .chain((listed.tool_calling == Some(false)).then(|| "no tools".to_owned()))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What the provider says a model is good at, with what it can do, for the
/// model choosing whom to hand a task to.
fn about(listed: &Listed, ranking: Option<&Ranking>) -> String {
    let can: Vec<&str> = [
        (listed.reasoning, "reasons"),
        (listed.vision, "reads images"),
    ]
    .into_iter()
    .filter_map(|(flag, what)| (flag == Some(true)).then_some(what))
    .collect();
    let description = listed.description.as_deref().unwrap_or_default().trim();
    let mut about = match (description.is_empty(), can.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("It {}.", can.join(", ")),
        (false, true) => description.to_owned(),
        (false, false) => format!("{description} It {}.", can.join(", ")),
    };
    if let Some(ranking) = ranking {
        let scores: Vec<String> = [
            score("coding", ranking.coding),
            score("intelligence", ranking.intelligence),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !scores.is_empty() {
            about.push_str(&format!(
                " Artificial Analysis scores out of 100, the same tests for every model: {}.",
                scores.join(", ")
            ));
        }
    }
    about.trim().to_owned()
}

/// The checks given on the command line. Without any, the project's own are
/// found each time they are needed: see [`ironquill_tools::detect_checks`].
fn checks_or_default(lines: Vec<String>) -> Result<Vec<Check>> {
    lines
        .iter()
        .map(|line| Check::parse(line).with_context(|| format!("empty check: {line:?}")))
        .collect()
}

async fn ask(provider: &OpenAiCompatible, prompt: &str, model: &str, effort: Effort) -> Result<()> {
    let model = ModelId::new(model)?;
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![Message::user(prompt)],
        tools: Vec::new(),
        effort: Some(effort),
    };

    // The price list is fetched alongside the answer rather than before it, so
    // knowing the cost never makes the answer arrive later.
    let (response, pricing) = tokio::join!(provider.complete(&request), provider.pricing(&model));
    let response = response.context("the request failed")?;

    println!("{}", response.content.as_deref().unwrap_or_default());
    eprintln!();
    eprintln!("Model:  {model}");
    eprintln!("Input:  {}", response.usage.input);
    eprintln!("Output: {}", response.usage.output);
    // The provider's own figure wins: it knows about caching and discounts.
    let cost = response
        .cost
        .or_else(|| pricing.as_ref().ok().map(|p| p.cost(&response.usage)));
    match cost {
        Some(cost) => eprintln!("Cost:   {cost}"),
        None => eprintln!("Cost:   unknown"),
    }
    Ok(())
}

async fn run_task(provider: &OpenAiCompatible, config: &AgentConfig, task: &str) -> Result<()> {
    let workspace = Workspace::new(".")?;
    let context = ironquill_tools::project_context(workspace.root(), FILE_LIST_LIMIT).await;

    let mut toolbox = Toolbox::new(workspace);
    let mut agents = Agents::find();
    let prices = prices().await;
    agents.claude = agents.claude.with_prices(prices.clone());
    let billed = agents.codex.uses_api_key();
    agents.codex = agents.codex.with_prices(prices).billed(billed);
    let outcome = ironquill_agent::run(
        provider,
        &agents,
        &mut toolbox,
        config,
        task,
        &context,
        show,
    )
    .await?;
    summarize(&outcome)
}

/// Whether streamed text left the cursor mid-line, so that the next event
/// starts on a line of its own.
static MID_LINE: AtomicBool = AtomicBool::new(false);

fn show(event: Event) {
    if let Event::Saying {
        text, new_block, ..
    } = &event
    {
        if *new_block {
            eprint!("\n  ");
        }
        eprint!("{text}");
        let _ = std::io::stderr().flush();
        MID_LINE.store(true, Ordering::Relaxed);
        return;
    }
    if MID_LINE.swap(false, Ordering::Relaxed) {
        eprintln!();
    }
    match event {
        Event::Saying { .. } => {}
        Event::Turn {
            model,
            usage,
            cost,
            subscription,
            context,
            ..
        } => {
            let cost = match cost {
                Some(c) => c.to_string(),
                None if subscription => "subscription".to_owned(),
                None => "cost ?".to_owned(),
            };
            let context = context.map(|c| format!("  {c}")).unwrap_or_default();
            eprintln!(
                "· {model}  in {}  out {}  {cost}{context}",
                usage.input, usage.output
            );
        }
        Event::Said { text, .. } => eprintln!("  {text}"),
        Event::Tool {
            name,
            path,
            outcome,
        } => {
            let path = path.unwrap_or_default();
            match outcome {
                // An agent's other tools are named with what they were given.
                Ok(ToolSummary::Ran { label, .. }) => eprintln!("    {label}"),
                Ok(_) => eprintln!("    {name} {path}"),
                Err(error) => eprintln!("    {name} {path}  ✗ {error}"),
            }
        }
        Event::Checking { commands } => eprintln!("▸ running {}", commands.join(", then ")),
        Event::Passed => eprintln!("✓ checks passed"),
        Event::Failed { command, .. } => eprintln!("✗ {command} failed"),
        Event::Escalating { from, to } => eprintln!("↑ {from} gave up, escalating to {to}"),
        Event::Delegating { from, to, task } => eprintln!("→ {from} hands to {to}: {task}"),
        Event::Step {
            number,
            of,
            name,
            model,
            effort,
        } => {
            let who = match (model, effort) {
                (Some(model), Some(effort)) => format!("{model} · effort {effort}"),
                (Some(model), None) => model.to_string(),
                (None, _) => "ironquill".to_owned(),
            };
            eprintln!("━━ {number}/{of} {name} · {who}");
        }
        Event::Tried { command, outcome } => {
            let first = outcome.lines().next().unwrap_or_default();
            eprintln!("· before any change, `{command}` {first}");
        }
        Event::PairEnded { text } => eprintln!("■ {text}"),
        Event::Command {
            command,
            status,
            checked_by,
            ..
        } => {
            let checked = checked_by.map_or_else(String::new, |m| format!(", checked by {m}"));
            eprintln!("$ {command}  ({status}{checked})");
        }
        Event::Silent { model } => eprintln!("✗ {model} answered nothing, twice"),
        Event::Progress { by, text, .. } => eprintln!("· where the work stands ({by}):\n{text}"),
        Event::OutOfTurns { model, turns } => eprintln!("· {model} used its {turns} turns"),
        Event::Restarted {
            model,
            before,
            after,
            ..
        } => eprintln!(
            "⇣ {model}'s cache expired: restarted from the summary, {before} → {after} tokens"
        ),
        Event::Notice { model, text } => eprintln!("· {model}: {text}"),
        Event::Denied {
            model,
            action,
            reason,
        } => eprintln!("⊘ {model}'s safety checks refused: {action} {reason}"),
        Event::Held {
            command, reasons, ..
        } => eprintln!(
            "⊘ refused, nobody to approve it: {command} ({})",
            reasons.join("; ")
        ),
        Event::Compacted {
            dropped,
            before,
            after,
        } => {
            eprintln!(
                "⇣ compacted: {dropped} old tool results dropped, about {before} → {after} tokens"
            );
        }
        Event::OverBudget { spent, budget } => {
            eprintln!("✗ budget of {budget} spent ({spent}), stopping");
        }
    }
}

fn summarize(outcome: &Outcome) -> Result<()> {
    eprintln!();
    if outcome.changed.is_empty() {
        eprintln!("Changed: nothing");
    } else {
        eprintln!("Changed: {}", outcome.changed.join(", "));
    }
    eprintln!(
        "Tokens:  in {}  out {}",
        outcome.usage.input, outcome.usage.output
    );
    let partial = if outcome.cost_complete {
        ""
    } else {
        " (some turns did not report a cost)"
    };
    eprintln!("Cost:    {}{partial}", outcome.cost);

    match &outcome.verdict {
        // `do` always runs the checks, so it never ends on a bare answer;
        // reporting one as a pass would claim a check that did not happen.
        Verdict::Answered => bail!("the model answered without changing anything"),
        Verdict::Unchecked => {
            eprintln!("Result:  changed, no check configured");
            Ok(())
        }
        Verdict::Passed { model } => {
            eprintln!("Result:  checks pass, change by {model}");
            Ok(())
        }
        Verdict::GaveUp { failure } => {
            if let Some(f) = failure {
                eprintln!("\nLast failure, `{}`:\n{}", f.command, f.excerpt);
            }
            bail!("no model made the checks pass")
        }
        Verdict::OverBudget { budget } => bail!("the budget of {budget} was spent first"),
    }
}
