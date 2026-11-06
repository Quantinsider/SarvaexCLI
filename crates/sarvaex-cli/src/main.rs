mod cli;
mod commands;
mod config;
mod output;

use clap::Parser;
use output::{Output, exit_code_for};

#[tokio::main]
async fn main() {
    let cli = cli::Cli::parse();

    // Logging always goes to stderr so stdout stays clean for JSON consumers and MCP stdio.
    let level = match cli.global.verbose {
        0 => "warn",
        1 => "info,sarvaex_sdk=debug,sarvaex_mcp=debug",
        _ => "debug,sarvaex_sdk=trace,sarvaex_mcp=trace,rmcp=debug",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();

    let out = Output::new(cli.global.json, cli.global.compact);
    let code = match commands::run(cli).await {
        Ok(code) => code,
        Err(err) => {
            out.error(&err);
            exit_code_for(&err)
        }
    };
    std::process::exit(code);
}
