use crate::cli::{Cli, Command};
use crate::config::Resolved;
use crate::output::{Output, exit};
use anyhow::Result;
use sarvaex_sdk::price::PriceEncoding;
use sarvaex_sdk::{Client, Contract};
use std::collections::HashMap;
use std::sync::Mutex;

pub mod account;
pub mod auth;
pub mod expansion;
pub mod markets;
pub mod mcp;
pub mod misc;
pub mod orders;
pub mod rfq;
pub mod stream;

pub struct Ctx {
    pub out: Output,
    pub resolved: Resolved,
    pub yes: bool,
    client: Client,
    contracts: Mutex<HashMap<String, Contract>>,
}

impl Ctx {
    pub fn new(out: Output, resolved: Resolved, yes: bool) -> Result<Self> {
        let client = resolved.client()?;
        Ok(Self {
            out,
            resolved,
            yes,
            client,
            contracts: Mutex::new(HashMap::new()),
        })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Fetch (and cache) a contract.
    pub async fn contract(&self, ticker: &str) -> Result<Contract> {
        let key = ticker.trim().to_ascii_uppercase();
        if let Some(c) = self.contracts.lock().unwrap().get(&key) {
            return Ok(c.clone());
        }
        let c = self.client.get_market(&key).await?;
        self.contracts.lock().unwrap().insert(key, c.clone());
        Ok(c)
    }

    /// Price encoding for a ticker; falls back to cents if the contract can't be fetched.
    pub async fn encoding(&self, ticker: &str) -> PriceEncoding {
        match self.contract(ticker).await {
            Ok(c) => PriceEncoding::for_contract(&c),
            Err(_) => PriceEncoding::binary(),
        }
    }

    /// Ask for confirmation when interactive; auto-approve otherwise.
    pub fn confirm(&self, prompt: &str) -> bool {
        if self.yes || !self.out.interactive() {
            return true;
        }
        eprint!("{prompt} [y/N] ");
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

pub async fn run(cli: Cli) -> Result<i32> {
    let out = Output::new(cli.global.json, cli.global.compact);

    // Commands that do not need a client or credential.
    match &cli.command {
        Command::Completions { shell } => {
            use clap::CommandFactory;
            clap_complete::generate(
                *shell,
                &mut Cli::command(),
                "sarvaex",
                &mut std::io::stdout(),
            );
            return Ok(exit::OK);
        }
        Command::Docs(cmd) => return misc::docs(&out, cmd),
        Command::Config(cmd) => return misc::config(&out, &cli.global, cmd),
        _ => {}
    }

    let resolved = Resolved::from_cli(&cli.global)?;
    let ctx = Ctx::new(out, resolved, cli.global.yes)?;

    match cli.command {
        Command::Auth(cmd) => auth::run(&ctx, cmd).await,
        Command::Markets(cmd) => markets::markets(&ctx, cmd).await,
        Command::Events(cmd) => markets::events(&ctx, cmd).await,
        Command::Series(cmd) => markets::series(&ctx, cmd).await,
        Command::Orders(cmd) => orders::run(&ctx, cmd).await,
        Command::Positions(cmd) => account::positions(&ctx, cmd).await,
        Command::Account(cmd) => account::account(&ctx, cmd).await,
        Command::Margin(cmd) => account::margin(&ctx, cmd).await,
        Command::Demo(cmd) => account::demo(&ctx, cmd).await,
        Command::Rfq(cmd) => rfq::run(&ctx, cmd).await,
        Command::MarketRequests(cmd) => misc::market_requests(&ctx, cmd).await,
        Command::Stream(args) => stream::run(&ctx, args).await,
        Command::Exchange(cmd) => expansion::exchange(&ctx, cmd).await,
        Command::Health => misc::health(&ctx).await,
        Command::Api(args) => misc::api(&ctx, args).await,
        Command::Mcp(cmd) => mcp::run(&ctx, cmd).await,
        Command::Completions { .. } | Command::Docs(_) | Command::Config(_) => unreachable!(),
    }
}
