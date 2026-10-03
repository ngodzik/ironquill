//! The `ironquill` command.

#![deny(unsafe_code)]

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ironquill_agent::{AgentConfig, Event, Member, Outcome, Verdict};
use ironquill_core::{ChatModel, ChatRequest, Message, ModelId, Usd};
use ironquill_llm::{Agents, ClaudeCode, Codex, Listed, OpenAiCompatible};
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

    /// The most one request may cost, in dollars. Past it the work stops
    /// and the model says where it is. Defaults to the one kept with
    /// /defaults, or 0.10.
    #[arg(long, env = "IRONQUILL_BUDGET")]
    budget: Option<f64>,

    /// Continue this project's most recent conversation.
    #[arg(short = 'c', long = "continue", conflicts_with = "resume")]
    continue_last: bool,

    /// Pick a saved conversation of this project to continue.
    #[arg(short = 'r', long)]
    resume: bool,

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
    let cli = Cli::parse();
    let api_key = cli.api_key.context("no API key: set IRONQUILL_API_KEY")?;
    let provider = OpenAiCompatible::new(cli.base_url, api_key);

    let Some(command) = cli.command else {
        // No log subscriber here: anything written to the terminal while the
        // interface owns it would tear the screen.
        let start = if cli.continue_last {
            ironquill_tui::Start::Continue
        } else if cli.resume {
            ironquill_tui::Start::Pick
        } else {
            ironquill_tui::Start::New
        };
        let choices = Choices {
            model: cli.model,
            escalate: cli.escalate,
            offered: cli.models,
            checks: cli.checks,
            budget: cli.budget,
        };
        return interface(provider, choices, start).await;
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    match command {
        Command::Ask { prompt, model } => ask(&provider, &prompt, &model).await,
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
            for check in checks_or_default(checks)? {
                builder = builder.check(check);
            }
            run_task(&provider, &builder.build()?, &task).await
        }
    }
}

/// What the command line chose for the interface; the rest comes from the
/// defaults kept with /defaults.
struct Choices {
    model: Option<String>,
    escalate: Vec<String>,
    offered: Vec<String>,
    checks: Vec<String>,
    budget: Option<f64>,
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

    let mut tiers = Vec::new();
    for id in choices
        .model
        .or(defaults.model)
        .into_iter()
        .chain(choices.escalate)
    {
        tiers.push(ModelId::new(id)?);
    }
    let checks = checks_or_default(choices.checks)?;

    let claude = ClaudeCode::find();
    let codex = Codex::find();
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
        .or(defaults.budget)
        .unwrap_or(Defaults::BUDGET);
    // The provider's list, to search and to price the team; without it the
    // interface still works, with less to show.
    let catalog = tokio::time::timeout(Duration::from_secs(10), provider.list())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|listed| {
            Some(Member {
                note: note(&listed),
                about: about(&listed),
                tools: listed.tool_calling != Some(false),
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
        catalog,
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

/// A few words on a listed model for the person and the model choosing:
/// `$0.14 / $0.28 per M tokens · 1M context`.
fn note(listed: &Listed) -> String {
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
    price
        .into_iter()
        .chain(window)
        .chain((listed.tool_calling == Some(false)).then(|| "no tools".to_owned()))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What the provider says a model is good at, with what it can do, for the
/// model choosing whom to hand a task to.
fn about(listed: &Listed) -> String {
    let can: Vec<&str> = [
        (listed.reasoning, "reasons"),
        (listed.vision, "reads images"),
    ]
    .into_iter()
    .filter_map(|(flag, what)| (flag == Some(true)).then_some(what))
    .collect();
    let description = listed.description.as_deref().unwrap_or_default().trim();
    match (description.is_empty(), can.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("It {}.", can.join(", ")),
        (false, true) => description.to_owned(),
        (false, false) => format!("{description} It {}.", can.join(", ")),
    }
}

fn checks_or_default(lines: Vec<String>) -> Result<Vec<Check>> {
    if lines.is_empty() {
        if Path::new("Cargo.toml").exists() {
            return Ok(["cargo check --all-targets", "cargo test"]
                .into_iter()
                .filter_map(Check::parse)
                .collect());
        }
        // Nothing known to check this kind of project: changes are kept as
        // written, as the verdict will say.
        return Ok(Vec::new());
    }
    lines
        .iter()
        .map(|line| Check::parse(line).with_context(|| format!("empty check: {line:?}")))
        .collect()
}

async fn ask(provider: &OpenAiCompatible, prompt: &str, model: &str) -> Result<()> {
    let model = ModelId::new(model)?;
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![Message::user(prompt)],
        tools: Vec::new(),
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
    let agents = Agents::find();
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
