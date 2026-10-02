//! The `ironquill` command.

#![deny(unsafe_code)]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ironquill_core::{ChatModel, ChatRequest, Message, ModelId};
use ironquill_llm::OpenAiCompatible;
use tracing_subscriber::EnvFilter;

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

    #[command(subcommand)]
    command: Command,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let api_key = cli.api_key.context("no API key: set IRONQUILL_API_KEY")?;
    let provider = OpenAiCompatible::new(cli.base_url, api_key);

    match cli.command {
        Command::Ask { prompt, model } => ask(&provider, &prompt, &model).await,
    }
}

async fn ask(provider: &OpenAiCompatible, prompt: &str, model: &str) -> Result<()> {
    let model = ModelId::new(model)?;
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![Message::user(prompt)],
    };

    // The price list is fetched alongside the answer rather than before it, so
    // knowing the cost never makes the answer arrive later.
    let (response, pricing) = tokio::join!(provider.complete(&request), provider.pricing(&model));
    let response = response.context("the request failed")?;

    println!("{}", response.content);
    eprintln!();
    eprintln!("Model:  {model}");
    eprintln!("Input:  {}", response.usage.input);
    eprintln!("Output: {}", response.usage.output);
    match pricing {
        Ok(pricing) => eprintln!("Cost:   {}", pricing.cost(&response.usage)),
        Err(error) => {
            tracing::debug!("no price for {model}: {error:#}");
            eprintln!("Cost:   unknown");
        }
    }
    Ok(())
}
