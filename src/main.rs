//! The `ironquill` command.

#![deny(unsafe_code)]

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ironquill_agent::{AgentConfig, Event, Outcome, Verdict};
use ironquill_core::{ChatModel, ChatRequest, Message, ModelId};
use ironquill_llm::OpenAiCompatible;
use ironquill_tools::{Check, Toolbox, Workspace};
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

    /// Checks for the interface. Defaults as for `do`.
    #[arg(long = "check", value_name = "COMMAND")]
    checks: Vec<String>,

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
        return interface(provider, cli.model, cli.escalate, cli.checks, start).await;
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

async fn interface(
    provider: OpenAiCompatible,
    model: Option<String>,
    escalate: Vec<String>,
    checks: Vec<String>,
    start: ironquill_tui::Start,
) -> Result<()> {
    let workspace = Workspace::new(".")?;

    let mut tiers = Vec::new();
    for id in model.into_iter().chain(escalate) {
        tiers.push(ModelId::new(id)?);
    }
    let checks = checks_or_default(checks)?;

    let settings = ironquill_tui::Settings {
        tiers,
        checks,
        rounds: 2,
        max_turns: 30,
    };
    ironquill_tui::run(Arc::new(provider), workspace, settings, start).await?;
    Ok(())
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
    let outcome =
        ironquill_agent::run(provider, &mut toolbox, config, task, &context, show).await?;
    summarize(&outcome)
}

fn show(event: Event) {
    match event {
        Event::Turn { model, usage, cost } => {
            let cost = cost.map_or_else(|| "cost ?".to_owned(), |c| c.to_string());
            eprintln!(
                "· {model}  in {}  out {}  {cost}",
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
                Ok(_) => eprintln!("    {name} {path}"),
                Err(error) => eprintln!("    {name} {path}  ✗ {error}"),
            }
        }
        Event::Checking { commands } => eprintln!("▸ running {}", commands.join(", then ")),
        Event::Passed => eprintln!("✓ checks passed"),
        Event::Failed { command, .. } => eprintln!("✗ {command} failed"),
        Event::Escalating { from, to } => eprintln!("↑ {from} gave up, escalating to {to}"),
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
    }
}
