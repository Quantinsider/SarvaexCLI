//! Command-line surface. Every command supports `--json`; when stdout is not a
//! terminal, JSON is the default so agents and scripts get stable output.

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "sarvaex",
    version,
    about = "SarvaEX prediction-market exchange CLI — built for humans and AI agents",
    long_about = "Trade binary YES/NO contracts and numeric futures on SarvaEX.\n\n\
Prices: `--price 0.51` or `51c` on binary contracts, `--price 4.30` on futures (contract display units), or `--price-ticks` for raw ticks.\n\
Output: human tables on a terminal, JSON when piped or with --json. Errors go to stderr as JSON.\n\
Exit codes: 0 ok · 1 error · 2 usage/validation · 3 auth · 4 order rejected · 5 rate limited · 6 network · 7 blocked by local safety guard.\n\n\
Run `sarvaex docs` for the full agent guide.",
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args, Clone)]
pub struct GlobalOpts {
    /// Config profile to use (see `sarvaex config`).
    #[arg(
        long,
        global = true,
        env = "SARVAEX_PROFILE",
        help_heading = "Global options"
    )]
    pub profile: Option<String>,

    /// Gateway base URL (default https://api.sarvaex.com; `local` = http://localhost:18080).
    #[arg(
        long,
        global = true,
        env = "SARVAEX_BASE_URL",
        help_heading = "Global options"
    )]
    pub base_url: Option<String>,

    /// WebSocket URL (derived from --base-url when omitted).
    #[arg(
        long,
        global = true,
        env = "SARVAEX_WS_URL",
        help_heading = "Global options"
    )]
    pub ws_url: Option<String>,

    /// API key (svx_live_…). Overrides the profile.
    #[arg(
        long,
        global = true,
        env = "SARVAEX_API_KEY",
        hide_env_values = true,
        help_heading = "Global options"
    )]
    pub api_key: Option<String>,

    /// Bearer token (JWT) instead of an API key.
    #[arg(
        long,
        global = true,
        env = "SARVAEX_TOKEN",
        hide_env_values = true,
        conflicts_with = "api_key",
        help_heading = "Global options"
    )]
    pub token: Option<String>,

    /// Always print JSON (default when stdout is not a TTY).
    #[arg(
        long,
        global = true,
        env = "SARVAEX_JSON",
        help_heading = "Global options"
    )]
    pub json: bool,

    /// Single-line JSON instead of pretty-printed.
    #[arg(long, global = true, help_heading = "Global options")]
    pub compact: bool,

    /// Request timeout in seconds.
    #[arg(
        long,
        global = true,
        default_value = "30",
        help_heading = "Global options"
    )]
    pub timeout: u64,

    /// Skip interactive confirmations.
    #[arg(short = 'y', long, global = true, help_heading = "Global options")]
    pub yes: bool,

    /// Verbose logging to stderr (-v debug, -vv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count, help_heading = "Global options")]
    pub verbose: u8,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Log in, manage API keys and credentials
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Discover markets and read market data
    #[command(subcommand)]
    Markets(MarketsCmd),
    /// Events group related contracts
    #[command(subcommand)]
    Events(EventsCmd),
    /// Series (catalog groupings)
    #[command(subcommand)]
    Series(SeriesCmd),
    /// Place, list and cancel orders
    #[command(subcommand)]
    Orders(OrdersCmd),
    /// Open positions and P&L
    #[command(subcommand)]
    Positions(PositionsCmd),
    /// Balance, risk, fills and ledger history
    #[command(subcommand)]
    Account(AccountCmd),
    /// Margin what-if preview
    #[command(subcommand)]
    Margin(MarginCmd),
    /// Demo-environment helpers (play money)
    #[command(subcommand)]
    Demo(DemoCmd),
    /// Request-for-quote workflow
    #[command(subcommand)]
    Rfq(RfqCmd),
    /// Propose new markets
    #[command(name = "market-requests", subcommand)]
    MarketRequests(MarketRequestsCmd),
    /// Stream live book updates, trades and your fills (NDJSON)
    Stream(StreamArgs),
    /// Gateway health overview
    Health,
    /// Raw API call (escape hatch): `sarvaex api GET /v1/markets?limit=1`
    Api(ApiArgs),
    /// Print the agent guide (SKILL.md) or install it into a project
    #[command(subcommand)]
    Docs(DocsCmd),
    /// Run the MCP server (stdio for Claude Desktop/Code; HTTP for Claude.ai/ChatGPT)
    #[command(subcommand)]
    Mcp(McpCmd),
    /// Manage config profiles
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Generate shell completions
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum AuthCmd {
    /// Sign in with email/user-id + password and mint a scoped API key for this machine
    Login(LoginArgs),
    /// Create a new SarvaEX account
    Register(RegisterArgs),
    /// Store an existing API key in the current profile
    SetKey {
        /// The key (svx_live_…); omit to read from stdin
        key: Option<String>,
    },
    /// Show the authenticated profile
    Whoami,
    /// Show which profile/credential/endpoint is active
    Status,
    /// Remove the stored credential (optionally revoke it on the exchange)
    Logout {
        /// Also revoke the API key on the exchange
        #[arg(long)]
        revoke: bool,
    },
    /// Manage API keys
    #[command(subcommand)]
    Keys(KeysCmd),
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Email or user_id (prompted if omitted)
    #[arg(long, short = 'u', env = "SARVAEX_IDENTIFIER")]
    pub identifier: Option<String>,
    /// Read the password from stdin instead of prompting (for scripts)
    #[arg(long)]
    pub password_stdin: bool,
    /// Name for the API key that will be created
    #[arg(long, default_value = "sarvaex-cli")]
    pub key_name: String,
    /// Create a read-only key (no trading:write)
    #[arg(long)]
    pub read_only: bool,
    /// Key expiry in days (default: no expiry)
    #[arg(long)]
    pub expires_in_days: Option<u32>,
    /// Print the key instead of storing it in the profile
    #[arg(long)]
    pub no_store: bool,
}

#[derive(Debug, Args)]
pub struct RegisterArgs {
    #[arg(long)]
    pub name: String,
    /// 3–32 chars
    #[arg(long)]
    pub user_id: String,
    #[arg(long)]
    pub email: String,
    #[arg(long)]
    pub password_stdin: bool,
    /// After registering, also mint and store an API key
    #[arg(long)]
    pub login: bool,
}

#[derive(Debug, Subcommand)]
pub enum KeysCmd {
    /// List API keys (prefixes and scopes; secrets are never shown)
    List,
    /// Create an API key (the secret is printed exactly once)
    Create {
        #[arg(long)]
        name: String,
        /// Comma-separated scopes; default: all (markets:read,account:read,orders:read,fills:read,trading:write,websocket:read)
        #[arg(long, value_delimiter = ',')]
        scopes: Option<Vec<String>>,
        /// RFC 3339 expiry
        #[arg(long)]
        expires_at: Option<String>,
    },
    /// Revoke a key by id
    Revoke { key_id: String },
}

// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum MarketsCmd {
    /// List contracts (default: OPEN)
    List(MarketListArgs),
    /// Free-text search across ticker, question and underlying
    Search {
        /// Words to match (all must appear)
        #[arg(required = true)]
        query: Vec<String>,
        /// State filter (default open; `any` for all)
        #[arg(long, default_value = "open")]
        state: String,
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Full details of one contract
    Get { ticker: String },
    /// Order-book snapshot
    Book {
        ticker: String,
        #[arg(long, default_value = "10")]
        depth: u32,
    },
    /// Recent public fills
    Fills {
        ticker: String,
        #[arg(long, default_value = "50")]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Open interest
    Oi { ticker: String },
    /// Settlement result (after settlement)
    Settlement { ticker: String },
}

#[derive(Debug, Args)]
pub struct MarketListArgs {
    /// open, closed, settled, …, or `any`
    #[arg(long, default_value = "open")]
    pub state: String,
    /// binary | futures
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub category: Option<String>,
    #[arg(long)]
    pub underlying: Option<String>,
    #[arg(long)]
    pub event: Option<String>,
    #[arg(long)]
    pub series: Option<String>,
    #[arg(long, default_value = "50")]
    pub limit: u32,
    #[arg(long)]
    pub cursor: Option<String>,
    /// Follow pagination and return everything
    #[arg(long)]
    pub all: bool,
    /// Also show contracts past their close time that are still waiting to be settled
    #[arg(long)]
    pub include_expired: bool,
}

#[derive(Debug, Subcommand)]
pub enum EventsCmd {
    List {
        #[arg(long)]
        series: Option<String>,
        #[arg(long, default_value = "100")]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
    Get {
        event_ticker: String,
    },
    /// Resolution status and outcome
    Resolution {
        event_ticker: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum SeriesCmd {
    List {
        #[arg(long, default_value = "100")]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
}

// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum OrdersCmd {
    /// Place an order
    Place(PlaceArgs),
    /// List your orders (default: working orders)
    List {
        #[arg(long)]
        ticker: Option<String>,
        /// open | partial | filled | cancelled | rejected | expired | pending | any (default open)
        #[arg(long, default_value = "open")]
        status: String,
        #[arg(long, default_value = "100")]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
        /// Follow pagination
        #[arg(long)]
        all: bool,
    },
    /// Fetch one order
    Get { order_id: String },
    /// Cancel one order
    Cancel {
        order_id: String,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Cancel all working orders
    CancelAll {
        #[arg(long)]
        ticker: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SideArg {
    Yes,
    No,
    Long,
    Short,
}

impl From<SideArg> for sarvaex_sdk::Side {
    fn from(s: SideArg) -> Self {
        match s {
            SideArg::Yes => Self::Yes,
            SideArg::No => Self::No,
            SideArg::Long => Self::Long,
            SideArg::Short => Self::Short,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ActionArg {
    Buy,
    Sell,
}

impl From<ActionArg> for sarvaex_sdk::Action {
    fn from(a: ActionArg) -> Self {
        match a {
            ActionArg::Buy => Self::Buy,
            ActionArg::Sell => Self::Sell,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TifArg {
    Gtc,
    Ioc,
    Fok,
}

impl From<TifArg> for sarvaex_sdk::Tif {
    fn from(t: TifArg) -> Self {
        match t {
            TifArg::Gtc => Self::Gtc,
            TifArg::Ioc => Self::Ioc,
            TifArg::Fok => Self::Fok,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StpArg {
    Unspecified,
    TakerAtCross,
    Maker,
}

impl From<StpArg> for sarvaex_sdk::Stp {
    fn from(s: StpArg) -> Self {
        match s {
            StpArg::Unspecified => Self::Unspecified,
            StpArg::TakerAtCross => Self::TakerAtCross,
            StpArg::Maker => Self::Maker,
        }
    }
}

#[derive(Debug, Args)]
pub struct PlaceArgs {
    pub ticker: String,
    /// yes|no (binary) or long|short (futures)
    #[arg(long, short = 's', value_enum)]
    pub side: SideArg,
    /// buy|sell
    #[arg(long, short = 'a', value_enum)]
    pub action: ActionArg,
    /// Number of contracts
    #[arg(long, short = 'n', alias = "qty")]
    pub count: i64,
    /// Human price: 0.51 / 51c (binary), 4.30 (futures display units)
    #[arg(long, short = 'p', conflicts_with_all = ["price_ticks", "market"])]
    pub price: Option<String>,
    /// Raw tick price
    #[arg(long, conflicts_with = "market")]
    pub price_ticks: Option<i64>,
    /// Market order (IOC at a gateway-derived protection price)
    #[arg(long, short = 'm')]
    pub market: bool,
    /// Time in force (ignored for --market, which is always IOC)
    #[arg(long, value_enum, default_value = "gtc")]
    pub tif: TifArg,
    /// Reject if the order would take liquidity
    #[arg(long)]
    pub post_only: bool,
    /// Only reduce an existing position
    #[arg(long)]
    pub reduce_only: bool,
    /// Self-trade prevention mode
    #[arg(long, value_enum)]
    pub stp: Option<StpArg>,
    /// RFC 3339 expiry
    #[arg(long)]
    pub expires_at: Option<String>,
    /// Your own id for reconciliation (≤128 chars). Reusing it on retry reuses the idempotency key.
    #[arg(long)]
    pub client_order_id: Option<String>,
    /// Explicit Idempotency-Key (to retry the exact same submission safely)
    #[arg(long)]
    pub idempotency_key: Option<String>,
    /// Validate and print the request without sending it
    #[arg(long)]
    pub dry_run: bool,
}

// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum PositionsCmd {
    List {
        #[arg(long)]
        include_closed: bool,
        #[arg(long, default_value = "100")]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
    Get {
        ticker: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountCmd {
    /// Cash, held and total balance
    Balance,
    /// Aggregate risk and configured limits
    Risk,
    /// Portfolio margin breakdown
    Margin,
    /// Profile
    Profile,
    /// Your fills
    Fills {
        #[arg(long)]
        ticker: Option<String>,
        #[arg(long)]
        order_id: Option<String>,
        #[arg(long, default_value = "100")]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Ledger entries
    History {
        #[arg(long, default_value = "100")]
        limit: u32,
        #[arg(long)]
        cursor: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum MarginCmd {
    /// Preview the margin impact of an order (read-only)
    Preview {
        ticker: String,
        #[arg(long, short = 's', value_enum)]
        side: SideArg,
        #[arg(long, short = 'a', value_enum)]
        action: ActionArg,
        #[arg(long, short = 'n')]
        count: i64,
        #[arg(long, short = 'p', conflicts_with = "price_ticks")]
        price: Option<String>,
        #[arg(long)]
        price_ticks: Option<i64>,
    },
}

#[derive(Debug, Subcommand)]
pub enum DemoCmd {
    /// Credit play-money USDC (demo/devnet only)
    Credit {
        /// Amount in USDC, e.g. 10000 or 2500.50
        #[arg(long)]
        usdc: String,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum RfqCmd {
    Create {
        ticker: String,
        #[arg(long, short = 's', value_enum)]
        side: SideArg,
        #[arg(long, short = 'a', value_enum)]
        action: ActionArg,
        #[arg(long, short = 'n')]
        count: i64,
        /// RFC 3339
        #[arg(long)]
        expires_at: String,
        #[arg(long)]
        client_rfq_id: Option<String>,
    },
    Get {
        rfq_id: String,
    },
    Quotes {
        rfq_id: String,
        #[arg(long, default_value = "100")]
        limit: u32,
    },
    /// Submit a two-sided quote (as a maker)
    Quote {
        rfq_id: String,
        /// Bid in human price (or use --bid-ticks)
        #[arg(long, conflicts_with = "bid_ticks")]
        bid: Option<String>,
        #[arg(long)]
        bid_ticks: Option<i64>,
        #[arg(long, conflicts_with = "offer_ticks")]
        offer: Option<String>,
        #[arg(long)]
        offer_ticks: Option<i64>,
        #[arg(long, short = 'n')]
        count: i64,
        #[arg(long)]
        expires_at: String,
        #[arg(long)]
        quote_id: Option<String>,
    },
    Accept {
        rfq_id: String,
        quote_id: String,
    },
    Cancel {
        rfq_id: String,
    },
    CancelQuote {
        rfq_id: String,
        quote_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum MarketRequestsCmd {
    List,
    Create {
        /// 10–300 characters
        #[arg(long)]
        question: String,
        /// BINARY | FUTURE
        #[arg(long)]
        kind: String,
        /// Economics | Finance | Crypto | Commodities | Climate | "Geopolitics / Shipping"
        #[arg(long)]
        category: String,
        #[arg(long)]
        source: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct StreamArgs {
    /// One or more tickers
    #[arg(required = true)]
    pub tickers: Vec<String>,
    /// Also subscribe to your private fills (needs websocket:read)
    #[arg(long)]
    pub private: bool,
    /// Maintain a local book and emit top-of-book snapshots instead of raw deltas
    #[arg(long)]
    pub book: bool,
    /// Stop after N events (0 = run until interrupted)
    #[arg(long, default_value = "0")]
    pub max_events: u64,
}

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// GET | POST | DELETE
    pub method: String,
    /// Path with optional query, e.g. /v1/markets?limit=1
    pub path: String,
    /// JSON body (string) or @file
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum DocsCmd {
    /// Print SKILL.md (default)
    Skill,
    /// Write the skill into a project: .claude/skills/sarvaex/SKILL.md (+ AGENTS.md section)
    Install {
        /// Project directory
        #[arg(long, default_value = ".")]
        dir: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum McpCmd {
    /// Serve MCP over stdio (for Claude Desktop, Claude Code, Cursor, …)
    Stdio,
    /// Serve MCP over HTTP with OAuth (for Claude.ai / ChatGPT custom connectors)
    Serve(ServeArgs),
    /// Print client configuration snippets
    Config {
        /// claude-desktop | claude-code | cursor | windsurf | remote
        #[arg(default_value = "claude-desktop")]
        target: String,
        /// Include the API key in the snippet (default: placeholder)
        #[arg(long)]
        with_key: bool,
    },
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    #[arg(long, env = "SARVAEX_MCP_BIND", default_value = "0.0.0.0:8787")]
    pub bind: String,
    /// Public origin clients will use, e.g. https://mcp.sarvaex.com
    #[arg(long, env = "SARVAEX_MCP_PUBLIC_URL")]
    pub public_url: String,
    /// Where to keep the SQLite store and server secret
    #[arg(long, env = "SARVAEX_MCP_STATE_DIR")]
    pub state_dir: Option<String>,
    /// Additional Host header values to accept
    #[arg(long)]
    pub allowed_host: Vec<String>,
    /// Accept any Host header (only behind a trusted proxy)
    #[arg(long)]
    pub allow_any_host: bool,
    /// Disable email/password sign-in on the consent page
    #[arg(long)]
    pub no_password_login: bool,
    /// Disable pasting an existing API key on the consent page
    #[arg(long)]
    pub no_api_key_paste: bool,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Show the config file path
    Path,
    /// Show the active configuration (secrets redacted)
    Show,
    /// Set the default profile
    Use { profile: String },
    /// Set a profile value: base_url | ws_url | user_id | read_only | max_order_notional_usdc | max_order_count | allowed_tickers
    Set { key: String, value: String },
    /// Delete a profile
    Delete { profile: String },
}
