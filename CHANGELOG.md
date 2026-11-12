# Changelog

Versions follow [semver](https://semver.org). The version lives in one place: `[workspace.package] version` in `Cargo.toml`.

## 0.3.0

Stop and take-profit orders for futures.

- `orders place --type stop-market | stop-limit | take-profit | take-profit-limit --trigger-price P`: the exchange places the order for you when the last trade price reaches the trigger. Futures only; reduce-only unless you add `--allow-open`; `-limit` types also take `--price`.
- `orders list --status trigger-pending` (also `trigger-fired`, `trigger-cancelled`, `trigger-rejected`, `trigger-expired`), `orders get` and `orders cancel` work on trigger orders, and `orders cancel-all` includes them.
- SDK: `OrderType` gains the four trigger types, `NewOrder::trigger(...)`, and `Order` carries `trigger_price_ticks` and `fired_order_id`.
- MCP: `place_order` accepts `order_type` STOP_MARKET, STOP_LIMIT, TAKE_PROFIT, TAKE_PROFIT_LIMIT with `trigger_price` (or `trigger_price_ticks`).
- OpenAPI describes the new order types, statuses and fields. The mock gateway accepts triggers.
- Needs a gateway with trigger orders (api.sarvaex.com has them). Older gateways answer `INVALID_ARGUMENT` to the new order types.

## 0.2.0

Everything the exchange added to its API, from the CLI, SDK and MCP server.

- `orders amend` (in place when possible, otherwise cancel and replace), `orders batch` (1-20 orders from a JSON file), `orders cancel-batch`, and `orders cancel-all` as one request (older gateways fall back to cancelling one by one).
- `positions close`: a reduce-only close of all or part of a position.
- `account cancel-after`: the dead man's switch, which cancels every working order unless refreshed.
- `account activity`, `account fees`, `account incentives`, `account settlements`.
- `exchange status | schedule | time | fees`: is trading open, what closes soon, the fee schedule.
- `markets candles | trades | stats | books`.
- `auth keys create` and `auth keys limits`: API keys with trading caps the exchange enforces (`--max-order-count`, `--max-order-notional-usdc`, `--max-daily-notional-usdc`, `--allow-ticker`, `--read-only`).
- `stream --orders --lifecycle --exchange --ticker-stats --candles`: the new WebSocket channels.
- MCP tools: `get_exchange_status`, `get_fee_schedule`, `get_market_stats`, `get_candles`, `get_account_activity`, `amend_order`, `cancel_orders`, `close_position`, `set_cancel_after`.
- OpenAPI spec 1.1.0 covers all of the above. The mock gateway implements them for offline testing.

## 0.1.0

First public release.

- `sarvaex` CLI: markets, order book, orders, positions, account, auth login.
- Yes/No contracts: both sides of the book, NO prices derived correctly, expired markets hidden and refused.
- MCP server (`sarvaex mcp stdio`) for Claude Code, Claude Desktop, Cursor and Windsurf.
- Local safety guards: read-only mode, order size and notional caps, ticker allow-list.
- Installers for macOS, Linux and Windows with checksum verification.
