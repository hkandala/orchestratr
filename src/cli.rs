use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Debug, Parser)]
#[command(name = "orcr", version, about = "Agent orchestration over herdr")]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Status,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Status) {
        Command::Status => {
            println!(
                "{}",
                json!({"ok": true, "result": {"herdr": "not yet wired"}})
            );
        }
    }
    Ok(())
}
