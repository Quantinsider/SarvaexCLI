---
name: sarvaex
description: Trade on SarvaEX, a prediction-market exchange with binary YES/NO contracts and numeric futures. Use when the user wants to discover markets, read order books, place or cancel orders, or check balance, positions and P&L on SarvaEX via the `sarvaex` CLI or the SarvaEX MCP tools.
---

# Trading on SarvaEX

SarvaEX lists two kinds of contracts:

- **Binary** (`kind = BINARY`, tickers `SX-…`): a YES/NO question. Prices are
  cents of a $1 payout, so `51` ticks = `$0.51` ≈ 51% implied probability. The
  NO price is always `100 − YES`. Sides are `YES` / `NO`.
- **Futures** (`kind = FUTURES`, tickers `SXF-…`): a numeric outcome (CPI %,
  BTC price, Fed funds rate). Sides are `LONG` / `SHORT`. Prices are integer
  ticks whose meaning comes from the contract's `settlement_rule.encoding`
  (`divider`, `decimals`, `prefix`, `suffix`): `430` ticks with divider 100 is
  `4.30%`. Never assume cents for futures.

Every action is `BUY` or `SELL` of a side. Quantities are whole contracts
(`count`). Money is micro-USDC (`÷ 1,000,000` for display). Nothing on the
exchange is a float.

## Non-negotiable rules

1. **Look before you trade.** Fetch the contract (`sarvaex markets get <ticker>`
   / `get_market`) and the book (`markets book` / `get_orderbook`) first. Only
   `state = OPEN` contracts accept orders.
2. **Sides must match the kind.** YES/NO for binary, LONG/SHORT for futures.
   The gateway rejects mismatches; the CLI checks locally first.
3. **HTTP 200 ≠ filled.** Read `order.status`, `filled_count`, `remaining_count`
   and `reject_code`. A rejected order (`POSITION_LIMIT_EXCEEDED`,
   `MAX_ORDER_SIZE_EXCEEDED`, `MARKET_NOT_AVAILABLE`, …) returns a normal
   response body with `reject_code` set. The CLI exits with code 4 in that case.
4. **Idempotency.** Each order/cancel needs a unique `Idempotency-Key`. The CLI
   generates one per invocation. If you must retry the *same* logical order
   after a timeout, pass the same `--idempotency-key` (or the same
   `--client-order-id`, which derives the key) so you never double-submit.
   `409 OPERATION_IN_PROGRESS` means "retry the same key shortly" — the CLI does
   this automatically.
5. **Market orders** are IOC with a protection price derived from the opposite
   side's live quote. They fail with `MARKET_NOT_AVAILABLE` when the book is
   empty on that side, and they can partially fill. Prefer limit orders with
   an explicit price when you need a predictable result.
6. **Respect limits.** Each contract has `max_order_size` and
   `position_limit_per_user`; the account has `max_order_size_micro_usdc` and
   `daily_loss_limit_micro_usdc` (`sarvaex account risk`). Use
   `sarvaex margin preview` (`preview_margin`) for the collateral impact of a
   futures order before placing it.
7. **Reconcile.** After placing orders, confirm with `sarvaex orders list
   --status OPEN`, `sarvaex positions list`, and `sarvaex account fills`.
8. **Stop when told.** If the operator set `SARVAEX_READ_ONLY=1`,
   `SARVAEX_MAX_ORDER_NOTIONAL_USDC`, or `SARVAEX_ALLOWED_TICKERS`, orders
   outside those bounds are blocked locally (exit code 7). Do not try to work
   around a guard; report it.
9. **Rate limits**: 1200 reads and 240 writes per minute per credential.
   Back off on `RATE_LIMITED` (the CLI already retries with `Retry-After`).
   Every order in a batch counts as one write.
10. **Check the exchange first.** `sarvaex exchange status` (`get_exchange_status`)
    says whether new orders are allowed: mode `OPEN`, `CANCEL_ONLY` (cancels only) or
    `CLOSED`, plus any maintenance window. Do not place orders while it is not `OPEN`.
11. **Running unattended? Arm the dead man's switch.** `sarvaex account cancel-after set 60`
    (`set_cancel_after`) cancels every working order if 60 s pass without a refresh;
    placing an order or running it again pushes the deadline out. Disarm with `off`.
12. **Amending can replace.** `orders amend` changes price and/or count. When the
    exchange cannot amend in place it cancels and re-submits (new order id, queue
    priority lost); the answer says which in `amended_via` (`IN_PLACE` or
    `CANCEL_REPLACE`). Pass `--no-cancel-replace` to fail instead.
13. **Batches are not atomic.** `orders batch` places up to 20 orders; read each
    result. `orders cancel-batch` cancels up to 50 ids. Failures exit with code 4/1.
14. **Fees.** Taker fees are 0 while trading is free; `sarvaex exchange fees` shows
    the schedule and `sarvaex account fees` what you have paid. Fees are charged on
    the price of the side you trade.
15. **Stops and take-profits (futures only).** `orders place … --type stop-market --trigger-price
    4.20` waits for the last trade price to reach the trigger, then the exchange places the
    order for you. Types: `stop-market`, `stop-limit`, `take-profit`, `take-profit-limit` (the
    `-limit` ones also need `--price`). They are reduce-only unless you add `--allow-open`. A
    trigger the market has already passed is refused (`TRIGGER_ALREADY_CROSSED`). The placed
    order has status `TRIGGER_PENDING`; `orders list --status trigger-pending` shows them and
    `orders cancel <id>` removes one. When one fires it becomes `TRIGGER_FIRED` and
    `fired_order_id` is the real order, so check that order, not the trigger, for the fill. A
    stop only protects you if the market trades through it: in a thin book a market stop can fill
    well away from the trigger price.
16. **Key limits are enforced by the exchange.** A key created with
    `--max-order-count`, `--max-order-notional-usdc`, `--max-daily-notional-usdc`,
    `--allow-ticker` or `--read-only` is refused with `KEY_LIMIT_EXCEEDED` (HTTP 403)
    beyond them. That is a boundary, not an error to retry around.

## Prices on the command line

- `--price 0.51` or `--price 51c` on a binary contract → 51 ticks.
- `--price 4.30` on a futures contract uses the contract's encoding → 430 ticks.
- `--price-ticks 51` bypasses conversion on any contract.
- Bare integers ≥ 1 are rejected on binary contracts as ambiguous.

## Quick reference

```bash
sarvaex auth login                            # email/user_id + password → mints a scoped API key
sarvaex markets list --state open --kind binary --limit 20
sarvaex markets search "fed rate"            # client-side search of question/ticker/underlying
sarvaex markets get SX-FEDDEC-26OCT-H25
sarvaex markets book SX-FEDDEC-26OCT-H25 --depth 10
sarvaex orders place SX-FEDDEC-26OCT-H25 --side yes --action buy --price 0.51 --count 10
sarvaex orders place SXF-USCPIYOY-26SEP --side long --action buy --price 4.30 --count 2 --tif IOC
sarvaex orders place SX-… --side no --action buy --market --count 5
sarvaex orders list --status open
sarvaex orders cancel <order_id>
sarvaex orders cancel-all --ticker SX-FEDDEC-26OCT-H25
sarvaex orders amend <order_id> --price 0.52              # in place when possible
sarvaex orders batch orders.json                          # 1-20 orders from a JSON file (or - for stdin)
sarvaex orders place SXF-… --side long --action sell --count 2 --type stop-market --trigger-price 4.20
sarvaex orders place SXF-… --side long --action sell --count 2 --type take-profit-limit --trigger-price 4.60 --price 4.55
sarvaex orders list --status trigger-pending              # stops and take-profits waiting to fire
sarvaex orders cancel-batch <id> <id> …                   # up to 50 ids
sarvaex positions list
sarvaex positions close SX-FEDDEC-26OCT-H25               # reduce-only; --price for a limit order
sarvaex account balance | risk | fills | history
sarvaex account activity --types fill,order --limit 20    # one feed of fills, orders, settlements, cash
sarvaex account fees | incentives | settlements
sarvaex account cancel-after set 60 | status | off        # dead man's switch
sarvaex exchange status | schedule | time | fees          # is trading open? what closes soon?
sarvaex markets stats SX-A SX-B                           # last price, 24h volume, best bid/ask
sarvaex markets candles SX-A --interval 1h --limit 24
sarvaex markets trades SX-A --from 2026-11-10T00:00:00Z
sarvaex markets books SX-A SX-B --depth 5                 # several books in one call
sarvaex auth keys create --name bot --max-order-count 20 --max-order-notional-usdc 50 --allow-ticker SX-A
sarvaex auth keys limits <key_id> --clear
sarvaex margin preview SXF-… --side long --action buy --price 4.30 --count 2
sarvaex stream SX-FEDDEC-26OCT-H25 --private  # NDJSON book deltas, trades, your fills
sarvaex stream --orders --exchange --lifecycle          # your order updates, exchange mode, market open/close
sarvaex stream SX-A --ticker-stats --candles 1m         # live statistics and candles
```

Add `--json` to any command for machine-readable output (automatic when stdout
is not a terminal). Errors are `{"error":{"code","message",...}}` on stderr.

Exit codes: `0` ok · `1` error · `2` usage · `3` auth · `4` order rejected ·
`5` rate limited · `6` network · `7` blocked by local safety guard.

## MCP tools (same semantics)

`list_markets`, `search_markets`, `get_market`, `get_orderbook`, `get_recent_fills`,
`get_open_interest`, `get_event`, `get_resolution`, `get_settlement`,
`get_balance`, `get_account_risk`, `get_account_margin`, `list_positions`,
`get_fill_history`, `get_ledger_history`, `list_orders`, `get_order`,
`place_order` (supports `dry_run`), `cancel_order`, `cancel_all_orders`,
`preview_margin`, `credit_demo_funds`, `get_trading_rules`, and for the expanded API:
`get_exchange_status`, `get_fee_schedule`, `get_market_stats`, `get_candles`,
`get_account_activity`, `amend_order`, `cancel_orders`, `close_position`,
`set_cancel_after`.

`place_order` accepts either `price` (human units like `0.51` or `4.30`) or
`price_ticks`. Always call it with `dry_run: true` first when the user has not
explicitly confirmed size and price.
