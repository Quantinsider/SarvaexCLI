# SarvaEX CLI & Agent Access — Implementation Plan

Status legend: ✅ done · 🔄 in progress · ⬜ not started

## 1. Goal

Let anyone connect an AI agent to SarvaEX and trade safely, in three ways:

1. **CLI** (`sarvaex`) — for scripts, bots and coding agents (Claude Code, Codex, Cursor).
2. **Local MCP server** (`sarvaex mcp stdio`) — for Claude Desktop, Claude Code, Cursor, Windsurf.
3. **Hosted MCP server** (`sarvaex mcp serve`) — for Claude.ai and ChatGPT web, where users add
   `https://mcp.sarvaex.com/mcp` as a custom connector and sign in with their SarvaEX account.

Success criteria

- An agent with only the CLI and `SKILL.md` can discover a market, read the book, place a
  correctly-priced order, and reconcile its position without human help.
- No code change to the exchange is required for v1; the client layer is a pure consumer of `gw-rest`/`gw-ws`.
- Every mutation is idempotent and every unsafe action can be bounded by the operator (read-only,
  notional cap, ticker allowlist) *outside* the agent's control.
- A Claude.ai or ChatGPT user can connect in under a minute and revoke access in one click.

## 2. Constraints found in the gateway (drive the design)

| # | Finding (source) | Consequence for the client |
|---|---|---|
| 1 | Default API-key scopes exclude `trading:write` and `websocket:read` (`default_api_key_scopes()`, gw-rest) | `auth login` must request scopes explicitly |
| 2 | JWT TTL 3600 s | Store only API keys, never JWTs |
| 3 | Rejected orders are `400` + `OrderResponse` body with `reject_code` (`order_rejection_status`) | Decode order bodies on 4xx; surface as exit code 4, not an exception |
| 4 | Futures prices need `settlement_rule.encoding` (seeded for all 96 contracts) | Exact integer encoding module; never assume cents |
| 5 | `side` validated against contract `kind` (1 binary / 2 futures), uppercase only | Validate locally before sending |
| 6 | `Idempotency-Key` required on all POSTs; `409 OPERATION_IN_PROGRESS` = retry same key; `409 IDEMPOTENCY_KEY_REUSED` on body mismatch | Auto-generate keys; derive deterministic key from `--client-order-id`; retry same key |
| 7 | Rate limit 1200 reads / 240 writes per minute per credential, `Retry-After` on 429 | Client-side backoff honouring `Retry-After` |
| 8 | Undocumented routes: `GET /v1/account/margin`, `POST /v1/account/margin/preview` (no idempotency key), `GET/POST /v1/market-requests`; extra contract fields `payoff_rule`, `payout_ticks`; event fields `outcome_grid`, `margin_model`, `trading_halted` | Modelled in SDK; `preview_margin` exposed as a what-if tool |
| 9 | Market orders = IOC at a protection price derived from the opposite side; `MARKET_NOT_AVAILABLE` on empty book | Explain in SKILL.md; prefer limits |
| 10 | gw-ws accepts `X-API-Key` on upgrade with `websocket:read` | WS client sends the key as an upgrade header |

## 3. Architecture

```
sarvaex-cli/  (Rust workspace, edition 2024, MSRV 1.89)
├── crates/sarvaex-sdk           typed client: models · price encoding · errors · retries · guards · WS
├── crates/sarvaex-cli           binary `sarvaex`: clap commands · profiles · JSON/table output · exit codes
├── crates/sarvaex-mcp           MCP tools (rmcp) · stdio transport · hosted HTTP + OAuth 2.1 · SQLite store
├── crates/sarvaex-mock-gateway  in-memory gw-rest clone for tests and offline agent development
├── SKILL.md                     agent-facing trading rules (embedded in CLI and MCP)
└── openapi.yaml                 public gateway spec for ChatGPT Actions / codegen
```

Design rules

- **Integers only.** Ticks, counts and micro-USDC flow through unchanged; conversion to display units
  happens at the edge and is exact (no `f64`).
- **One client, many credentials.** `Client::with_swapped_credential` lets the hosted MCP server act as
  each connected user with one connection pool.
- **Local validation before network.** Contract state, side/kind, tick grid, bounds and max size are
  checked client-side so agents get precise, actionable errors and the gateway sees fewer bad requests.
- **Guards are operator-owned.** `SARVAEX_READ_ONLY`, `SARVAEX_MAX_ORDER_NOTIONAL_USDC`,
  `SARVAEX_MAX_ORDER_COUNT`, `SARVAEX_ALLOWED_TICKERS` are read from env/profile, not from agent input.
- **Stable machine contract.** JSON on stdout when not a TTY, errors as `{"error":{code,message}}` on
  stderr, exit codes `0 ok · 1 error · 2 usage · 3 auth · 4 rejected · 5 rate-limited · 6 network · 7 guard`.

## 4. Phases

### Phase 1 — SDK ✅
- [x] Models for every route in API.md plus the undocumented ones
- [x] `PriceEncoding` from `settlement_rule.encoding` → `divider` → kind default; exact parse/format
- [x] `Client` with retries (429/5xx/409-in-progress, `Retry-After`), idempotent POST retries only
- [x] `validate_order` + `Guards`
- [x] `cancel_all_orders` (pages through OPEN/PARTIAL/PENDING)
- [x] WebSocket client + `LocalBook` with `book_seq` gap detection
- [x] 14 unit tests

### Phase 2 — CLI ✅
- [x] `auth login|register|set-key|whoami|status|logout|keys {list,create,revoke}`
- [x] `markets list|search|get|book|fills|oi|settlement`, `events`, `series`
- [x] `orders place|list|get|cancel|cancel-all` with `--price`/`--price-ticks`/`--market`, `--dry-run`, confirmation on TTY
- [x] `positions`, `account balance|risk|margin|profile|fills|history`, `margin preview`, `demo credit`
- [x] `rfq create|get|quotes|quote|accept|cancel|cancel-quote`, `market-requests list|create`
- [x] `stream` (NDJSON; `--book` maintains a local book and resyncs on gaps)
- [x] `api` raw escape hatch, `health`, `docs skill|install`, `config`, `completions`
- [x] Profiles in `~/.config/sarvaex/config.toml` (0600), env overrides, guards summary in `auth status`

### Phase 3 — MCP over stdio ✅
- [x] 23 tools with JSON Schema and read-only/destructive annotations
- [x] `place_order` with human `price` or `price_ticks`, `dry_run`, authoritative status in result
- [x] Trading rules as server `instructions` and `sarvaex://docs/trading-rules` resource
- [x] `sarvaex mcp config claude-desktop|claude-code|cursor|windsurf|remote` snippets

### Phase 4 — Hosted MCP with OAuth 2.1 ✅
- [x] Streamable HTTP, stateless + JSON responses (restart-safe, horizontally scalable)
- [x] `401` + `WWW-Authenticate: Bearer resource_metadata=…`
- [x] RFC 9728 protected-resource metadata, RFC 8414 AS metadata, RFC 7591 dynamic client registration
- [x] Consent page: SarvaEX password sign-in (mints a per-connection API key) or paste an existing key; "Allow trading" → `trade` scope
- [x] PKCE S256 required; single-use 10-min codes; 1 h access tokens; 30 d rotating refresh tokens; RFC 7009 revoke (also revokes the SarvaEX key)
- [x] SQLite store (WAL); tokens/codes stored hashed; API keys XChaCha20-Poly1305 encrypted under `SARVAEX_MCP_SECRET`
- [x] Direct `Authorization: Bearer svx_live_…` accepted for header-capable clients
- [x] Host-header validation from `--public-url`

### Phase 5 — Testing ✅ (live smoke test pending)
- [x] Mock gateway reproducing auth, scopes, idempotency replay, 400+OrderResponse rejections, 429, 409-in-progress, and a gw-ws-compatible `/ws`
- [x] 34 tests green under `cargo test --workspace`: 14 SDK unit · 11 SDK integration (incl. WebSocket book/gap) · 3 CLI black-box · 3 MCP store · 2 MCP OAuth/HTTP · 1 doctest
- [x] `cargo clippy --workspace --all-targets -D warnings` clean
- [ ] Smoke test against the real `https://api.sarvaex.com` with a demo account (needs network from CI or a dev machine) — see §4a

### Phase 6 — Packaging & distribution 🔄
- [x] `README.md` with install, quick start, guards, agent setup for Claude Code / Claude Desktop / Claude.ai / ChatGPT
- [x] `openapi.yaml` (OpenAPI 3.1, 39 operations) generated by `scripts/gen_openapi.py`; CI fails if it drifts
- [x] `deploy/Dockerfile` (multi-stage, non-root) + `deploy/docker-compose.mcp.yml` (server + Caddy TLS) + `deploy/claude_desktop_config.example.json`
- [x] GitHub Actions: `ci.yml` (fmt, clippy -D warnings, tests, OpenAPI freshness) and `release.yml` (Linux x86_64/aarch64, macOS arm/x86, Windows)
- [ ] First tagged release `sarvaex-v0.1.0` → binaries on GitHub Releases
- [ ] `cargo install sarvaex-cli` (publish `sarvaex-sdk`, `sarvaex-mcp`, `sarvaex-cli` to crates.io) and a `curl | sh` installer
- [ ] Homebrew tap

### Phase 4a — Live smoke test checklist (run once against production with a demo account) ⬜
```bash
sarvaex auth login --identifier <demo user>          # mints a key with trading:write + websocket:read
sarvaex markets list --limit 5 && sarvaex markets get SXF-USCPIYOY-26SEP   # confirm encoding renders e.g. 4.30%
sarvaex markets book SX-FEDDEC-26OCT-H25
sarvaex orders place SX-FEDDEC-26OCT-H25 --side yes --action buy --price 0.01 --count 1 --dry-run
sarvaex orders place SX-FEDDEC-26OCT-H25 --side yes --action buy --price 0.01 --count 1   # deep resting order
sarvaex orders list && sarvaex orders cancel-all --ticker SX-FEDDEC-26OCT-H25 -y
sarvaex stream SX-FEDDEC-26OCT-H25 --private --book --max-events 5
sarvaex mcp stdio   # then connect Claude Desktop and run get_trading_rules + list_markets
```

### Phase 7 — Rollout ⬜
- [ ] DNS + TLS for `mcp.sarvaex.com` (Caddy or ALB in front of `sarvaex mcp serve`; Claude.ai and ChatGPT require HTTPS)
- [ ] Set `SARVAEX_MCP_SECRET`, persistent volume for `/var/lib/sarvaex-mcp`
- [ ] Add a "Connect an AI agent" section to `frontend/TradingApiPage.jsx` with the three connection paths
- [ ] Publish `SKILL.md` to the Claude Code / Codex skill registries once available
- [ ] Monitor: `/healthz`, gateway 401/403/429 rates per API key prefix

### Phase 8 — Recommended backend follow-ups (not blocking)
- [ ] Document the routes/fields in §2 row 8 in `API.md`
- [ ] gw-rest caches API keys in memory at startup; a second replica will not see keys minted on the first → load on miss or subscribe to a NATS invalidation
- [ ] Rate limiter is per-process; move to a shared store if `gw-rest` is replicated
- [ ] Consider including `websocket:read` in default API-key scopes
- [ ] `services/migrations/run.sh` applies only migrations 000001–000009; confirm the production path for 000010–000020 and the contract-catalog seed

## 5. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Agent double-submits after a timeout | Deterministic idempotency key from `--client-order-id`; SKILL.md rule 4; `409 OPERATION_IN_PROGRESS` auto-retry |
| Agent mis-prices a futures contract | Human prices parsed via contract encoding; bare integers rejected on binaries; `--dry-run` prints the exact request |
| Runaway agent | Operator guards enforced in-process before any request; exit code 7 is distinct and documented |
| Hosted MCP compromise | API keys encrypted at rest, tokens hashed, per-connection keys revocable from the SarvaEX profile page, read-only consent by default |
| Claude.ai / ChatGPT OAuth quirks | Metadata served at both `/.well-known/...` and `/.well-known/.../mcp`; public and confidential clients supported; PKCE enforced |
| Protocol drift in MCP | Built on the official Rust SDK (`rmcp`); stateless transport avoids session-store coupling |

## 6. Decision log

- **Rust over TypeScript/Python** — matches the backend team's skills, single static binary, same toolchain in CI; MCP via the official Rust SDK.
- **Single binary** (`sarvaex`) hosting CLI, stdio MCP and the remote server — simplest install story; the remote server can be split into its own binary later without code changes (it lives in `sarvaex-mcp`).
- **OAuth AS built into the MCP server** rather than reusing gateway JWTs — Claude.ai/ChatGPT need standards-based discovery + PKCE; the gateway has no OAuth endpoints; keeping it client-side means zero backend changes for v1.
- **Per-connection SarvaEX API keys** rather than storing passwords or long-lived JWTs — revocable, scoped, visible in the user's key list.
- **Stateless Streamable HTTP** — no server-initiated notifications are needed; gains restart safety and horizontal scaling.
- **SQLite for the MCP store** — one small table set, single-node deployment for v1; swap to Postgres if the connector base grows.
