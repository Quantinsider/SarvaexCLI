# sarvaex — CLI, SDK and MCP server for the SarvaEX exchange

Trade binary YES/NO contracts and numeric futures on [SarvaEX](https://api.sarvaex.com) from a
terminal, a script, or an AI agent.

| Component | What it is | Who uses it |
|---|---|---|
| `sarvaex` CLI | Human-friendly on a terminal, JSON when piped, stable exit codes | People, scripts, Claude Code / Codex / Cursor agents |
| `sarvaex mcp stdio` | Local MCP server with 23 tools | Claude Desktop, Claude Code, Cursor, Windsurf |
| `sarvaex mcp serve` | Hosted MCP server with OAuth 2.1 | Claude.ai and ChatGPT custom connectors |
| `sarvaex-sdk` | Typed Rust client (REST + WebSocket) | Your own Rust bots |
| `sarvaex-mock-gateway` | In-memory gateway clone | Testing agents offline |

Everything speaks the public gateway only — no privileged access, no backend changes.

## Install

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/Quantinsider/SarvaexCLI/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/Quantinsider/SarvaexCLI/main/install.ps1 | iex
```

Both download the release for your machine, check its SHA-256 checksum and put `sarvaex` on your PATH
(`~/.local/bin` on macOS and Linux). You can also download an archive from the
[Releases page](https://github.com/Quantinsider/SarvaexCLI/releases).

With Rust 1.89+ installed, build from source instead:

```bash
cargo install --git https://github.com/Quantinsider/SarvaexCLI sarvaex-cli
```

Then create an account on the Sarvaex website and run `sarvaex auth login`.

## Quick start

```bash
sarvaex auth login                       # email/user_id + password → mints a scoped API key for this machine
sarvaex markets list --kind binary       # OPEN markets
sarvaex markets search "fed rate"
sarvaex markets book SX-FEDDEC-26OCT-H25
sarvaex orders place SX-FEDDEC-26OCT-H25 --side yes --action buy --price 0.51 --count 10 --dry-run
sarvaex orders place SX-FEDDEC-26OCT-H25 --side yes --action buy --price 0.51 --count 10
sarvaex positions list
sarvaex account balance
```

Prices are typed in display units: `0.51` or `51c` on binary contracts, `4.30` on a percentage
futures contract, `--price-ticks 51` for raw ticks. Money is shown in USDC.

Add `--json` for machine output (automatic when stdout is not a TTY). Errors are JSON on stderr:

```json
{"error":{"code":"VALIDATION","message":"side LONG is invalid for a BINARY contract; use YES or NO"}}
```

Exit codes: `0` ok · `1` error · `2` usage/validation · `3` auth · `4` order rejected by the
exchange · `5` rate limited · `6` network · `7` blocked by a local safety guard.

## More commands

```bash
sarvaex exchange status                  # is trading open? (OPEN / CANCEL_ONLY / CLOSED, maintenance window)
sarvaex exchange schedule | time | fees
sarvaex orders amend <order_id> --price 0.52          # in place when possible, else cancel + replace
sarvaex orders batch orders.json                      # 1-20 orders from a JSON file (or - for stdin)
sarvaex orders cancel-batch <id> <id> …               # up to 50 ids in one call
sarvaex orders cancel-all [--ticker SX-…]             # one request on current gateways
sarvaex positions close SX-…                          # reduce-only; --price for a limit order
sarvaex account activity --types fill,order           # fills, orders, settlements and cash in one feed
sarvaex account fees | incentives | settlements
sarvaex account cancel-after set 60                   # dead man's switch: cancel everything unless refreshed
sarvaex markets stats | candles | trades | books      # market data beyond the book
sarvaex stream --orders --exchange --lifecycle        # live order updates, exchange mode, market open/close
```

A batch file is a JSON array; prices are in display units like everywhere else:

```json
[
  {"ticker": "SX-FEDDEC-26OCT-H25", "side": "yes", "action": "buy", "price": "0.48", "count": 5},
  {"ticker": "SX-FEDDEC-26OCT-H25", "side": "yes", "action": "sell", "price_ticks": 55, "count": 5, "tif": "gtc"}
]
```

### API keys with trading caps

Create keys the exchange itself will hold to a limit, for bots and agents:

```bash
sarvaex auth keys create --name bot --max-order-count 20 --max-order-notional-usdc 50 \
  --max-daily-notional-usdc 500 --allow-ticker SX-FEDDEC-26OCT-H25
sarvaex auth keys limits <key_id> --read-only          # or --clear to remove every limit
```

An order beyond a limit is refused with `403 KEY_LIMIT_EXCEEDED`. A key with limits can only
create keys that are at least as restricted as itself.

## Safety guards for agents

Set these in the environment (or `sarvaex config set …`) and the client refuses to send anything
outside them, before any request leaves the machine. Agents cannot override them.

| Variable | Effect |
|---|---|
| `SARVAEX_READ_ONLY=1` | Block every mutating call |
| `SARVAEX_MAX_ORDER_NOTIONAL_USDC=500` | Reject orders whose estimated notional exceeds the cap |
| `SARVAEX_MAX_ORDER_COUNT=50` | Reject orders above this size |
| `SARVAEX_ALLOWED_TICKERS=SX-A,SX-B` | Only allow orders on these tickers |

## Connecting AI agents

### Claude Code / Codex / Cursor (CLI + skill)

```bash
cd your-project
sarvaex docs install          # writes .claude/skills/sarvaex/SKILL.md and an AGENTS.md section
sarvaex mcp config claude-code   # prints the `claude mcp add …` command for the stdio server
```

The skill tells the agent how prices work, that HTTP 200 ≠ filled, how to retry safely with
`--client-order-id`, and which exit codes mean what.

### Claude Desktop / Cursor / Windsurf (local MCP)

```bash
sarvaex mcp config claude-desktop --with-key
```

Paste the printed JSON into the client's MCP config. The server runs `sarvaex mcp stdio` with your
stored API key.

### Claude.ai and ChatGPT (hosted MCP)

Run the hosted server behind HTTPS (both platforms require it):

```bash
sarvaex mcp serve --public-url https://mcp.sarvaex.com --bind 0.0.0.0:8787 \
  --state-dir /var/lib/sarvaex-mcp
```

or with Docker: `docker compose -f deploy/docker-compose.mcp.yml up -d` (set `SARVAEX_MCP_SECRET`).

Then users add `https://mcp.sarvaex.com/mcp` as a custom connector:

* **Claude.ai** — Settings → Connectors → Add custom connector.
* **ChatGPT** — Settings → Connectors → Create (developer mode), authentication *OAuth*.

They are sent to a consent page where they sign in with their SarvaEX account (or paste an API
key) and choose whether to **allow trading**. The server mints a dedicated, scoped SarvaEX API key
for that connection, stores it encrypted, and revokes it when the user disconnects. Read-only
connections can browse markets and positions but every trading tool refuses.

Header-capable MCP clients can skip OAuth: `Authorization: Bearer svx_live_…`.

## MCP tools

`list_markets`, `search_markets`, `get_market`, `get_orderbook`, `get_recent_fills`,
`get_open_interest`, `get_event`, `get_resolution`, `get_settlement`, `get_balance`,
`get_account_risk`, `get_account_margin`, `list_positions`, `get_fill_history`,
`get_ledger_history`, `list_orders`, `get_order`, `place_order` (with `dry_run`), `cancel_order`,
`cancel_all_orders`, `preview_margin`, `credit_demo_funds`, `get_trading_rules`, plus
`get_exchange_status`, `get_fee_schedule`, `get_market_stats`, `get_candles`,
`get_account_activity`, `amend_order`, `cancel_orders`, `close_position`, `set_cancel_after`.

## Developing

```bash
cargo test --workspace                       # 33 tests incl. OAuth flow against the mock gateway
cargo run -p sarvaex-mock-gateway            # http://127.0.0.1:18080 with demo keys printed
SARVAEX_BASE_URL=http://127.0.0.1:18080 SARVAEX_API_KEY=<printed key> sarvaex markets list
```

Layout, decisions and remaining work: [`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).
Agent-facing rules: [`SKILL.md`](SKILL.md). Gateway spec: [`openapi.yaml`](openapi.yaml).

## Configuration

`~/.config/sarvaex/config.toml` (override with `SARVAEX_CONFIG`):

```toml
default_profile = "default"

[profiles.default]
base_url = "https://api.sarvaex.com"
api_key = "svx_live_…"
user_id = "alice_1"
max_order_notional_usdc = "500"

[profiles.local]
base_url = "http://localhost:18080"
```

Flags and environment variables override the profile: `--profile`, `--base-url`/`SARVAEX_BASE_URL`
(`local` is shorthand for the docker-compose gateway), `--api-key`/`SARVAEX_API_KEY`,
`--token`/`SARVAEX_TOKEN`.
