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

```bash
# from source (Rust 1.89+)
cargo install --path crates/sarvaex-cli

# or build the whole workspace
cargo build --release          # → target/release/sarvaex
```

Pre-built binaries are published on the GitHub Releases page by the release workflow in `.github/workflows/`.

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
`cancel_all_orders`, `preview_margin`, `credit_demo_funds`, `get_trading_rules`.

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
