//! The SarvaEX MCP server: tools, resources and credential resolution.
//!
//! One `SarvaexMcp` instance is created per MCP session. In stdio mode it uses
//! the operator's configured credential. In remote (HTTP) mode the auth
//! middleware stores a [`Principal`] in the request extensions and every tool
//! call acts as that user.

use crate::SKILL_MD;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use sarvaex_sdk::price::{self, PriceEncoding};
use sarvaex_sdk::{
    Action, Client, Contract, Credential, FillFilter, MarketFilter, NewOrder, OrderFilter,
    OrderType, Side, Tif,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub const RULES_URI: &str = "sarvaex://docs/trading-rules";

/// Who a remote MCP request acts as. Inserted into the HTTP request extensions
/// by the auth middleware and read back inside tool calls.
#[derive(Debug, Clone)]
pub struct Principal {
    pub credential: Credential,
    pub user_id: Option<String>,
    /// False when the user connected without granting trading permission.
    pub can_trade: bool,
    pub grant_id: Option<String>,
}

/// Shared, immutable server state.
#[derive(Debug)]
pub struct McpState {
    /// Base client: endpoint, timeouts, guards. In stdio mode it also carries the credential.
    pub base: Client,
    /// Human description shown in server info.
    pub environment_label: String,
}

#[derive(Clone)]
pub struct SarvaexMcp {
    state: Arc<McpState>,
    tool_router: ToolRouter<Self>,
}

impl SarvaexMcp {
    pub fn new(state: Arc<McpState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    /// Resolve the client to use for this request.
    fn client(&self, ctx: &RequestContext<RoleServer>) -> Result<(Client, bool), McpError> {
        if let Some(parts) = ctx.extensions.get::<http::request::Parts>() {
            return match parts.extensions.get::<Principal>() {
                Some(p) => Ok((
                    self.state
                        .base
                        .with_swapped_credential(p.credential.clone()),
                    p.can_trade,
                )),
                None => Err(McpError::invalid_request(
                    "request is not authenticated",
                    None,
                )),
            };
        }
        if self.state.base.credential().is_some() {
            Ok((self.state.base.clone(), true))
        } else {
            // Public market data still works without a credential.
            Ok((self.state.base.clone(), false))
        }
    }

    fn trading_client(&self, ctx: &RequestContext<RoleServer>) -> Result<Client, McpError> {
        let (client, can_trade) = self.client(ctx)?;
        if !client.credential().is_some() {
            return Err(McpError::invalid_request(
                "no credential configured: set SARVAEX_API_KEY (or run `sarvaex auth login`) before using account or trading tools",
                None,
            ));
        }
        if !can_trade {
            return Err(McpError::invalid_request(
                "this connection was authorised as read-only; reconnect and tick “Allow trading” to place or cancel orders",
                None,
            ));
        }
        Ok(client)
    }
}

// ---------------------------------------------------------------------------
// Parameter types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListMarketsParams {
    /// Market state filter. Default `OPEN`. One of DRAFT, LISTED, OPEN, HALTED, CLOSED, RESOLVING, SETTLED, CANCELLED, or `ANY`.
    pub state: Option<String>,
    /// `binary` (YES/NO) or `futures` (LONG/SHORT numeric contracts).
    pub kind: Option<String>,
    /// Settlement category, e.g. Economics, Finance, Crypto, Commodities, Climate, Geopolitics / Shipping.
    pub category: Option<String>,
    /// Case-insensitive underlying search, e.g. `bitcoin`.
    pub underlying: Option<String>,
    pub event_ticker: Option<String>,
    pub series_ticker: Option<String>,
    /// 1–500, default 50.
    pub limit: Option<u32>,
    /// Pagination cursor from a previous response.
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchMarketsParams {
    /// Free-text search over ticker, question and underlying (case-insensitive, all words must match).
    pub query: String,
    /// Restrict to a market state (default OPEN; `ANY` for all).
    pub state: Option<String>,
    /// Max results, default 20.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TickerParams {
    /// Contract ticker, e.g. `SX-FEDDEC-26OCT-H25` or `SXF-USCPIYOY-26SEP`.
    pub ticker: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OrderbookParams {
    pub ticker: String,
    /// Levels per side, 1–100 (default 10).
    pub depth: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecentFillsParams {
    pub ticker: String,
    /// Default 50.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EventParams {
    /// Event ticker, e.g. `XEV-STD-ECO-01`.
    pub event_ticker: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListPositionsParams {
    /// Include closed (zero-quantity) positions for realized P&L history.
    pub include_closed: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FillHistoryParams {
    pub ticker: Option<String>,
    pub order_id: Option<String>,
    /// Default 50.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LedgerParams {
    /// Default 50.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListOrdersParams {
    pub ticker: Option<String>,
    /// PENDING, OPEN, PARTIAL, FILLED, CANCELLED, REJECTED or EXPIRED. Default: OPEN.
    pub status: Option<String>,
    /// Default 50.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OrderIdParams {
    pub order_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PlaceOrderParams {
    pub ticker: String,
    /// `YES`/`NO` for binary contracts, `LONG`/`SHORT` for futures.
    pub side: String,
    /// `BUY` or `SELL`.
    pub action: String,
    /// Number of contracts (positive integer).
    pub count: i64,
    /// Human price in the contract's display units, e.g. `"0.51"` or `"51c"` for binary, `"4.30"` for a percentage futures contract. Omit for market orders.
    pub price: Option<String>,
    /// Raw integer tick price; alternative to `price`.
    pub price_ticks: Option<i64>,
    /// `LIMIT` (default) or `MARKET`.
    pub order_type: Option<String>,
    /// `GTC` (default), `IOC` or `FOK`.
    pub tif: Option<String>,
    /// Reject instead of taking liquidity.
    pub post_only: Option<bool>,
    /// Only reduce an existing position.
    pub reduce_only: Option<bool>,
    /// Caller-chosen id for reconciliation (≤128 chars). Reusing the same id on a retry also reuses the idempotency key.
    pub client_order_id: Option<String>,
    /// If true, validate and show the exact request without sending it.
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CancelAllParams {
    /// Only cancel orders on this ticker.
    pub ticker: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CandlesParams {
    pub ticker: String,
    /// `1m`, `5m`, `15m`, `1h` (default), `4h` or `1d`.
    pub interval: Option<String>,
    /// Start of the range (RFC 3339 or unix seconds).
    pub from: Option<String>,
    /// End of the range, exclusive.
    pub to: Option<String>,
    /// Candles to return, 1–1000 (default 100).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ActivityParams {
    /// Comma list of `fill`, `order`, `settlement`, `cash` (default all).
    pub types: Option<String>,
    pub from: Option<String>,
    /// Exclusive upper bound; pass the previous page's `next_to` to page back.
    pub to: Option<String>,
    pub ticker: Option<String>,
    /// 1–200 (default 50).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AmendOrderParams {
    pub order_id: String,
    /// New price in display units (`0.51`, `51c`, futures display units).
    pub price: Option<String>,
    /// New price in raw ticks (alternative to `price`).
    pub price_ticks: Option<i64>,
    /// New total contract count.
    pub count: Option<i64>,
    /// Default true: when the order cannot be amended in place, cancel it and
    /// submit a replacement (queue priority is lost). Set false to fail instead.
    pub allow_cancel_replace: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CancelOrdersParams {
    /// 1–50 order ids.
    pub order_ids: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClosePositionParams {
    pub ticker: String,
    /// Contracts to close (default: the whole position).
    pub count: Option<i64>,
    /// Limit price in display units; omit for a market order.
    pub price: Option<String>,
    /// Limit price in raw ticks (alternative to `price`).
    pub price_ticks: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CancelAfterParams {
    /// 1–3600 arms the dead man's switch; 0 disarms it.
    pub seconds: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PreviewMarginParams {
    pub ticker: String,
    pub side: String,
    pub action: String,
    pub count: i64,
    pub price: Option<String>,
    pub price_ticks: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreditDemoParams {
    /// Whole USDC to credit (demo environments only).
    pub amount_usdc: i64,
}

// ---------------------------------------------------------------------------
// Output helpers
// ---------------------------------------------------------------------------

fn ok(value: Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

fn tool_err(e: sarvaex_sdk::SarvaexError) -> Result<CallToolResult, McpError> {
    let mut v = e.to_json();
    v["error"]["hint"] = json!(hint_for(&e));
    Ok(CallToolResult::error(vec![ContentBlock::text(
        v.to_string(),
    )]))
}

fn hint_for(e: &sarvaex_sdk::SarvaexError) -> &'static str {
    match e.code() {
        "UNAUTHENTICATED" => "The credential is missing or invalid. Reconnect / re-authenticate.",
        "PERMISSION_DENIED" | "FORBIDDEN" => {
            "The API key lacks the required scope (trading:write for orders)."
        }
        "RATE_LIMITED" => "Slow down; wait for retry_after_seconds before calling again.",
        "MARKET_NOT_AVAILABLE" => {
            "No live quote on the opposite side; use a limit order with an explicit price."
        }
        "VALIDATION" => "Fix the request parameters; see message.",
        "GUARD" => {
            "A local safety limit set by the operator blocked this. Report it; do not work around it."
        }
        "NOT_FOUND" => "Check the ticker / id with list_markets or list_orders.",
        _ => "See message. Verify state with list_orders / list_positions before retrying.",
    }
}

fn compact_contract(c: &Contract) -> Value {
    let enc = PriceEncoding::for_contract(c);
    json!({
        "ticker": c.ticker,
        "kind": c.kind().as_str(),
        "state": c.state().as_str(),
        "question": c.question,
        "underlying": c.underlying,
        "category": c.category(),
        "event_ticker": c.event_ticker,
        "close_at": c.close_at,
        "expected_resolution_at": c.expected_resolution_at,
        "sides": match c.kind() { sarvaex_sdk::ContractKind::Binary => "YES/NO", _ => "LONG/SHORT" },
        "price_format": format!("1 tick = {}; range {}–{}", enc.format(1), enc.format(c.min_price_ticks.max(1)), enc.format(c.max_price_ticks)),
        "max_order_size": c.max_order_size,
    })
}

fn full_contract(c: &Contract) -> Value {
    let enc = PriceEncoding::for_contract(c);
    let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
    v["kind_name"] = json!(c.kind().as_str());
    v["state_name"] = json!(c.state().as_str());
    v["price_encoding"] = json!(enc);
    v["price_examples"] = json!({
        "min": enc.format(c.min_price_ticks.max(1)),
        "max": enc.format(c.max_price_ticks),
        "one_tick": enc.format(1),
    });
    v
}

fn book_json(c: &Contract, book: &sarvaex_sdk::OrderBook) -> Value {
    let enc = PriceEncoding::for_contract(c);
    let level = |l: &sarvaex_sdk::BookLevel| json!({ "price_ticks": l.price_ticks, "price": enc.format(l.price_ticks), "qty": l.total_qty, "orders": l.order_count });
    let mut bids: Vec<_> = book.bids.iter().collect();
    bids.sort_by_key(|l| std::cmp::Reverse(l.price_ticks));
    let mut asks: Vec<_> = book.asks.iter().collect();
    asks.sort_by_key(|l| l.price_ticks);
    let mut v = json!({
        "ticker": book.ticker,
        "seq": book.seq,
        "ts": book.ts,
        "best_bid": book.best_bid().map(level),
        "best_ask": book.best_ask().map(level),
        "bids": bids.iter().map(|l| level(l)).collect::<Vec<_>>(),
        "asks": asks.iter().map(|l| level(l)).collect::<Vec<_>>(),
    });
    if c.kind() == sarvaex_sdk::ContractKind::Binary {
        v["note"] = json!(
            "Prices are YES prices in cents. NO price = 100 − YES. Buying NO at 49 is equivalent to selling YES at 51."
        );
        if let Some(ba) = book.best_ask() {
            v["implied_probability_pct"] = json!(ba.price_ticks);
        }
    }
    v
}

fn parse_side_action(
    side: &str,
    action: &str,
) -> Result<(Side, Action), sarvaex_sdk::SarvaexError> {
    let s: Side = side
        .parse()
        .map_err(sarvaex_sdk::SarvaexError::Validation)?;
    let a: Action = action
        .parse()
        .map_err(sarvaex_sdk::SarvaexError::Validation)?;
    Ok((s, a))
}

fn resolve_price(
    c: &Contract,
    price: Option<&str>,
    ticks: Option<i64>,
) -> Result<i64, sarvaex_sdk::SarvaexError> {
    match (ticks, price) {
        (Some(t), _) => Ok(t),
        (None, Some(p)) => PriceEncoding::for_contract(c).parse(p),
        (None, None) => Err(sarvaex_sdk::SarvaexError::Validation(
            "provide `price` or `price_ticks` for a limit order".into(),
        )),
    }
}

fn parse_state(
    s: Option<&str>,
) -> Result<Option<sarvaex_sdk::ContractState>, sarvaex_sdk::SarvaexError> {
    match s.map(str::trim) {
        None | Some("") => Ok(Some(sarvaex_sdk::ContractState::Open)),
        Some(v) if v.eq_ignore_ascii_case("any") || v.eq_ignore_ascii_case("all") => Ok(None),
        Some(v) => v
            .parse()
            .map(Some)
            .map_err(sarvaex_sdk::SarvaexError::Validation),
    }
}

macro_rules! try_tool {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(err) => return tool_err(err.into()),
        }
    };
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl SarvaexMcp {
    #[tool(
        description = "List SarvaEX markets (contracts). Defaults to OPEN markets. Returns a compact summary per contract plus next_cursor for pagination.",
        annotations(title = "List markets", read_only_hint = true, open_world_hint = true)
    )]
    async fn list_markets(
        &self,
        Parameters(p): Parameters<ListMarketsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let state = try_tool!(parse_state(p.state.as_deref()));
        let kind = match p.kind.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(k) => Some(try_tool!(
                k.parse::<sarvaex_sdk::ContractKind>()
                    .map_err(sarvaex_sdk::SarvaexError::Validation)
            )),
            None => None,
        };
        let filter = MarketFilter {
            state,
            kind,
            series_ticker: p.series_ticker,
            event_ticker: p.event_ticker,
            underlying: p.underlying,
            category: p.category,
            limit: Some(p.limit.unwrap_or(50).clamp(1, 500)),
            cursor: p.cursor,
            ..Default::default()
        };
        let page = try_tool!(client.list_markets(&filter).await);
        ok(json!({
            "count": page.items.len(),
            "markets": page.items.iter().map(compact_contract).collect::<Vec<_>>(),
            "next_cursor": page.next_cursor,
        }))
    }

    #[tool(
        description = "Search markets by free text across ticker, question and underlying. Use this when the user names a topic (e.g. 'fed rate', 'bitcoin', 'CPI') rather than a ticker.",
        annotations(
            title = "Search markets",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn search_markets(
        &self,
        Parameters(p): Parameters<SearchMarketsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let state = try_tool!(parse_state(p.state.as_deref()));
        let all = try_tool!(
            client
                .list_all_markets(
                    &MarketFilter {
                        state,
                        ..Default::default()
                    },
                    10
                )
                .await
        );
        let words: Vec<String> = p
            .query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let limit = p.limit.unwrap_or(20).clamp(1, 200);
        let hits: Vec<Value> = all
            .iter()
            .filter(|c| {
                let hay = format!(
                    "{} {} {} {}",
                    c.ticker,
                    c.question,
                    c.underlying,
                    c.category().unwrap_or("")
                )
                .to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .take(limit)
            .map(compact_contract)
            .collect();
        ok(json!({ "query": p.query, "count": hits.len(), "markets": hits }))
    }

    #[tool(
        description = "Get full details for one contract: question, kind, state, tick/price encoding, bounds, limits, close and resolution times, settlement rule. Call this before placing an order.",
        annotations(title = "Get market", read_only_hint = true, open_world_hint = true)
    )]
    async fn get_market(
        &self,
        Parameters(p): Parameters<TickerParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let c = try_tool!(client.get_market(p.ticker.trim()).await);
        ok(full_contract(&c))
    }

    #[tool(
        description = "Order-book snapshot for a contract with prices rendered in display units, best bid/ask, and sequence number.",
        annotations(
            title = "Get order book",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_orderbook(
        &self,
        Parameters(p): Parameters<OrderbookParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let ticker = p.ticker.trim();
        let c = try_tool!(client.get_market(ticker).await);
        let book = try_tool!(client.orderbook(ticker, Some(p.depth.unwrap_or(10))).await);
        ok(book_json(&c, &book))
    }

    #[tool(
        description = "Recent public trades (fills) on a contract, newest first as returned by the exchange.",
        annotations(title = "Recent fills", read_only_hint = true, open_world_hint = true)
    )]
    async fn get_recent_fills(
        &self,
        Parameters(p): Parameters<RecentFillsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let ticker = p.ticker.trim();
        let c = try_tool!(client.get_market(ticker).await);
        let enc = PriceEncoding::for_contract(&c);
        let page = try_tool!(
            client
                .market_fills(
                    ticker,
                    None,
                    None,
                    Some(p.limit.unwrap_or(50).clamp(1, 500)),
                    None
                )
                .await
        );
        ok(json!({
            "ticker": ticker,
            "fills": page.items.iter().map(|f| json!({
                "ts": f.ts, "price_ticks": f.price_ticks, "price": enc.format(f.price_ticks), "count": f.count,
                "aggressor_side": f.aggressor_side, "global_seq": f.global_seq,
            })).collect::<Vec<_>>(),
            "next_cursor": page.next_cursor,
        }))
    }

    #[tool(
        description = "Open interest (total open long/short contracts) for a contract.",
        annotations(title = "Open interest", read_only_hint = true, open_world_hint = true)
    )]
    async fn get_open_interest(
        &self,
        Parameters(p): Parameters<TickerParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let oi = try_tool!(client.open_interest(p.ticker.trim()).await);
        ok(serde_json::to_value(oi).unwrap_or(Value::Null))
    }

    #[tool(
        description = "Event metadata (an event groups related contracts): title, description, expected resolution, margin model, outcome grid.",
        annotations(title = "Get event", read_only_hint = true, open_world_hint = true)
    )]
    async fn get_event(
        &self,
        Parameters(p): Parameters<EventParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let e = try_tool!(client.get_event(p.event_ticker.trim()).await);
        ok(serde_json::to_value(e).unwrap_or(Value::Null))
    }

    #[tool(
        description = "Resolution status and outcome for an event (attestations, sources, timestamps).",
        annotations(
            title = "Get resolution",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_resolution(
        &self,
        Parameters(p): Parameters<EventParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        ok(try_tool!(
            client.get_resolution(p.event_ticker.trim()).await
        ))
    }

    #[tool(
        description = "Settlement result for a contract once settled: payout per contract, total payout, positions settled.",
        annotations(
            title = "Get settlement",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_settlement(
        &self,
        Parameters(p): Parameters<TickerParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        ok(try_tool!(client.get_settlement(p.ticker.trim()).await))
    }

    // ---- account ---------------------------------------------------------

    #[tool(
        description = "Account cash, held collateral and total balance in micro-USDC and USDC.",
        annotations(title = "Get balance", read_only_hint = true)
    )]
    async fn get_balance(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let b = try_tool!(client.balance().await);
        ok(json!({
            "user_id": b.user_id,
            "cash_usdc": price::micro_to_usdc(b.cash_micro_usdc),
            "held_usdc": price::micro_to_usdc(b.held_micro_usdc),
            "total_usdc": price::micro_to_usdc(b.total_micro_usdc),
            "raw": b,
        }))
    }

    #[tool(
        description = "Aggregate account risk: collateral, equity, realized/unrealized P&L, open positions/orders, and the configured risk limits.",
        annotations(title = "Account risk", read_only_hint = true)
    )]
    async fn get_account_risk(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let r = try_tool!(client.account_risk().await);
        let mut v = serde_json::to_value(&r).unwrap_or(Value::Null);
        v["display"] = json!({
            "available_collateral_usdc": price::micro_to_usdc(r.available_collateral_micro_usdc),
            "equity_usdc": price::micro_to_usdc(r.equity_micro_usdc),
            "realized_pnl_usdc": price::micro_to_usdc(r.realized_pnl_micro_usdc),
            "unrealized_pnl_usdc": price::micro_to_usdc(r.unrealized_pnl_micro_usdc),
        });
        ok(v)
    }

    #[tool(
        description = "Portfolio (cross-event) margin breakdown per event for futures positions.",
        annotations(title = "Account margin", read_only_hint = true)
    )]
    async fn get_account_margin(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        ok(try_tool!(client.account_margin().await))
    }

    #[tool(
        description = "Open positions with net quantity, average cost and P&L (micro-USDC and USDC).",
        annotations(title = "List positions", read_only_hint = true)
    )]
    async fn list_positions(
        &self,
        Parameters(p): Parameters<ListPositionsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let page = try_tool!(
            client
                .positions(p.include_closed.unwrap_or(false), Some(500), None)
                .await
        );
        ok(json!({
            "positions": page.items.iter().map(|pos| json!({
                "ticker": pos.ticker,
                "net_qty": pos.net_qty,
                "avg_cost_usdc": price::micro_to_usdc(pos.avg_cost_micro_usdc),
                "realized_pnl_usdc": price::micro_to_usdc(pos.realized_pnl_micro_usdc),
                "unrealized_pnl_usdc": price::micro_to_usdc(pos.unrealized_pnl_micro_usdc),
                "updated_at": pos.updated_at,
                "raw": pos,
            })).collect::<Vec<_>>(),
            "next_cursor": page.next_cursor,
        }))
    }

    #[tool(
        description = "The authenticated user's own fills (executions), optionally filtered by ticker or order id. Use for reconciliation.",
        annotations(title = "Fill history", read_only_hint = true)
    )]
    async fn get_fill_history(
        &self,
        Parameters(p): Parameters<FillHistoryParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let page = try_tool!(
            client
                .account_fills(&FillFilter {
                    ticker: p.ticker,
                    order_id: p.order_id,
                    limit: Some(p.limit.unwrap_or(50).clamp(1, 500)),
                    ..Default::default()
                })
                .await
        );
        ok(json!({ "fills": page.items, "next_cursor": page.next_cursor }))
    }

    #[tool(
        description = "Ledger history: deposits, holds, fills, fees and settlements posted to the account.",
        annotations(title = "Ledger history", read_only_hint = true)
    )]
    async fn get_ledger_history(
        &self,
        Parameters(p): Parameters<LedgerParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let page = try_tool!(
            client
                .account_history(Some(p.limit.unwrap_or(50).clamp(1, 500)), None)
                .await
        );
        ok(json!({
            "entries": page.items.iter().map(|e| json!({
                "posted_at": e.posted_at, "direction": e.direction, "amount_usdc": price::micro_to_usdc(e.amount_micro_usdc),
                "running_balance_usdc": price::micro_to_usdc(e.running_balance_micro_usdc), "reason_code": e.reason_code,
                "memo": e.memo, "account_code": e.account_code, "tx_id": e.tx_id,
            })).collect::<Vec<_>>(),
            "next_cursor": page.next_cursor,
        }))
    }

    // ---- orders ----------------------------------------------------------

    #[tool(
        description = "List the user's orders. Defaults to working (OPEN) orders; pass status to see others.",
        annotations(title = "List orders", read_only_hint = true)
    )]
    async fn list_orders(
        &self,
        Parameters(p): Parameters<ListOrdersParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let status = match p.status.as_deref().map(str::trim) {
            None | Some("") => Some("OPEN".to_owned()),
            Some(s) if s.eq_ignore_ascii_case("any") || s.eq_ignore_ascii_case("all") => None,
            Some(s) => Some(s.to_ascii_uppercase()),
        };
        let page = try_tool!(
            client
                .list_orders(&OrderFilter {
                    ticker: p.ticker,
                    status,
                    limit: Some(p.limit.unwrap_or(50).clamp(1, 500)),
                    cursor: None
                })
                .await
        );
        ok(json!({ "orders": page.items, "next_cursor": page.next_cursor }))
    }

    #[tool(
        description = "Fetch one order by id (authoritative status, filled/remaining counts, average fill price).",
        annotations(title = "Get order", read_only_hint = true)
    )]
    async fn get_order(
        &self,
        Parameters(p): Parameters<OrderIdParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let o = try_tool!(client.get_order(p.order_id.trim()).await);
        ok(serde_json::to_value(o).unwrap_or(Value::Null))
    }

    #[tool(
        description = "Place an order. Validates the contract is OPEN, the side matches the kind, and the price is on the tick grid before sending. Returns the authoritative order status — HTTP success does not mean filled; check `status`, `filled_count`, `remaining_count`, `reject_code`. Set dry_run=true to preview the exact request without sending.",
        annotations(
            title = "Place order",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn place_order(
        &self,
        Parameters(p): Parameters<PlaceOrderParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.trading_client(&ctx)?;
        let ticker = p.ticker.trim();
        let (side, action) = try_tool!(parse_side_action(&p.side, &p.action));
        let order_type: OrderType = match p
            .order_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(t) => try_tool!(
                t.parse::<OrderType>()
                    .map_err(sarvaex_sdk::SarvaexError::Validation)
            ),
            None if p.price.is_none() && p.price_ticks.is_none() => OrderType::Market,
            None => OrderType::Limit,
        };
        let contract = try_tool!(client.get_market(ticker).await);
        let enc = PriceEncoding::for_contract(&contract);
        let mut order = if order_type == OrderType::Market {
            NewOrder::market(ticker, side, action, p.count)
        } else {
            let ticks = try_tool!(resolve_price(&contract, p.price.as_deref(), p.price_ticks));
            NewOrder::limit(ticker, side, action, ticks, p.count)
        };
        if let Some(t) = p.tif.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            order.tif = try_tool!(
                t.parse::<Tif>()
                    .map_err(sarvaex_sdk::SarvaexError::Validation)
            );
        }
        order.post_only = p.post_only.unwrap_or(false);
        order.reduce_only = p.reduce_only.unwrap_or(false);
        let idem = p
            .client_order_id
            .as_deref()
            .map(|id| sarvaex_sdk::idempotency::idempotency_key_for("order", id));
        if let Some(id) = p.client_order_id {
            order.client_order_id = id;
        }
        // Validate (contract state, side/kind, price grid, guards) before anything leaves.
        try_tool!(client.validate_order(&order).await);
        let display_price = if order.order_type == OrderType::Market {
            "MARKET".to_owned()
        } else {
            enc.format(order.price_ticks)
        };
        if p.dry_run.unwrap_or(false) {
            return ok(json!({
                "dry_run": true,
                "would_send": order,
                "display": { "ticker": ticker, "side": side, "action": action, "price": display_price, "count": p.count,
                              "estimated_notional_usdc": price::estimated_notional_micro(&contract, if order.order_type == OrderType::Market { 0 } else { order.price_ticks }, order.count).map(price::micro_to_usdc) },
                "note": "Nothing was sent. Call again with dry_run=false to submit.",
            }));
        }
        let resp = try_tool!(client.place_order_unchecked(&order, idem.as_deref()).await);
        let v = json!({
            "status": resp.order.status,
            "summary": resp.summary(),
            "rejected": resp.is_rejected(),
            "reject_code": resp.reject_code,
            "reject_reason": resp.reject_reason,
            "order": resp.order,
            "fills": resp.fills,
            "display": { "price": display_price, "avg_fill_price": if resp.order.avg_fill_price_ticks > 0 { enc.format(resp.order.avg_fill_price_ticks) } else { String::new() } },
        });
        if resp.is_rejected() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                serde_json::to_string_pretty(&v).unwrap_or_default(),
            )]));
        }
        ok(v)
    }

    #[tool(
        description = "Cancel one working order by id. Returns the updated order with cancelled_count.",
        annotations(
            title = "Cancel order",
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn cancel_order(
        &self,
        Parameters(p): Parameters<OrderIdParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.trading_client(&ctx)?;
        let resp = try_tool!(client.cancel_order(p.order_id.trim(), None).await);
        let v = json!({ "summary": resp.summary(), "rejected": resp.is_rejected(), "order": resp.order, "reject_code": resp.reject_code, "reject_reason": resp.reject_reason });
        if resp.is_rejected() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                v.to_string(),
            )]));
        }
        ok(v)
    }

    #[tool(
        description = "Cancel all working orders, optionally only on one ticker.",
        annotations(
            title = "Cancel all orders",
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn cancel_all_orders(
        &self,
        Parameters(p): Parameters<CancelAllParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.trading_client(&ctx)?;
        let (cancelled, failed) = try_tool!(
            client
                .cancel_all_orders(p.ticker.as_deref().map(str::trim))
                .await
        );
        ok(
            json!({ "cancelled": cancelled, "failed": failed.iter().map(|(id, why)| json!({"order_id": id, "reason": why})).collect::<Vec<_>>() }),
        )
    }

    #[tool(
        description = "What-if margin impact of an order on its event (before/after/delta in micro-USDC). Read-only; does not place anything.",
        annotations(title = "Preview margin", read_only_hint = true)
    )]
    async fn preview_margin(
        &self,
        Parameters(p): Parameters<PreviewMarginParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        let ticker = p.ticker.trim();
        let (side, action) = try_tool!(parse_side_action(&p.side, &p.action));
        let contract = try_tool!(client.get_market(ticker).await);
        let ticks = try_tool!(resolve_price(&contract, p.price.as_deref(), p.price_ticks));
        let m = try_tool!(
            client
                .preview_margin(ticker, side, action, ticks, p.count)
                .await
        );
        let mut v = serde_json::to_value(&m).unwrap_or(Value::Null);
        v["display"] = json!({
            "margin_before_usdc": price::micro_to_usdc(m.margin_before_micro_usdc),
            "margin_after_usdc": price::micro_to_usdc(m.margin_after_micro_usdc),
            "delta_usdc": price::micro_to_usdc(m.delta_micro_usdc),
        });
        ok(v)
    }

    #[tool(
        description = "Demo/devnet only: credit play-money USDC to the account. Fails on production.",
        annotations(title = "Credit demo funds", destructive_hint = false)
    )]
    async fn credit_demo_funds(
        &self,
        Parameters(p): Parameters<CreditDemoParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.trading_client(&ctx)?;
        if p.amount_usdc <= 0 {
            return tool_err(sarvaex_sdk::SarvaexError::Validation(
                "amount_usdc must be positive".into(),
            ));
        }
        let micro = try_tool!(
            p.amount_usdc
                .checked_mul(1_000_000)
                .ok_or_else(|| sarvaex_sdk::SarvaexError::Validation("amount too large".into()))
        );
        let r = try_tool!(
            client
                .demo_credit(micro, Some("mcp demo funding"), None)
                .await
        );
        ok(
            json!({ "ok": r.ok, "balance_usdc": price::micro_to_usdc(r.balance.total_micro_usdc), "balance": r.balance }),
        )
    }

    #[tool(
        description = "Is the exchange accepting orders? Returns mode OPEN, CANCEL_ONLY or CLOSED, whether new orders and cancels are allowed, and any maintenance window. Check this before trading.",
        annotations(
            title = "Exchange status",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_exchange_status(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        ok(try_tool!(client.exchange_status().await))
    }

    #[tool(
        description = "The fee schedule: taker fee and maker incentive in basis points (0 while trading is free), and how the fee is computed.",
        annotations(title = "Fee schedule", read_only_hint = true, open_world_hint = true)
    )]
    async fn get_fee_schedule(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        ok(try_tool!(client.fee_schedule().await))
    }

    #[tool(
        description = "Market statistics: last price, 24h volume/trades/high/low/change, best bid/ask and open interest. Prices are in ticks.",
        annotations(title = "Market stats", read_only_hint = true, open_world_hint = true)
    )]
    async fn get_market_stats(
        &self,
        Parameters(p): Parameters<TickerParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        ok(try_tool!(client.market_stats(p.ticker.trim()).await))
    }

    #[tool(
        description = "OHLC candles for a market, oldest first (prices in ticks; buckets without trades are omitted).",
        annotations(title = "Candles", read_only_hint = true, open_world_hint = true)
    )]
    async fn get_candles(
        &self,
        Parameters(p): Parameters<CandlesParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        ok(try_tool!(
            client
                .candles(
                    p.ticker.trim(),
                    p.interval.as_deref().unwrap_or("1h"),
                    p.from.as_deref(),
                    p.to.as_deref(),
                    Some(p.limit.unwrap_or(100).clamp(1, 1000)),
                )
                .await
        ))
    }

    #[tool(
        description = "Newest-first feed of everything that happened on the account: fills, order updates, settlements and cash movements. Page back with next_to.",
        annotations(title = "Account activity", read_only_hint = true)
    )]
    async fn get_account_activity(
        &self,
        Parameters(p): Parameters<ActivityParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let (client, _) = self.client(&ctx)?;
        ok(try_tool!(
            client
                .account_activity(
                    p.types.as_deref(),
                    p.from.as_deref(),
                    p.to.as_deref(),
                    p.ticker.as_deref(),
                    Some(p.limit.unwrap_or(50).clamp(1, 200)),
                )
                .await
        ))
    }

    #[tool(
        description = "Change a working order's price and/or count. If it cannot be amended in place it is cancelled and replaced (new order id, queue priority lost) unless allow_cancel_replace is false. The result's amended_via says which happened; verify with get_order.",
        annotations(
            title = "Amend order",
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn amend_order(
        &self,
        Parameters(p): Parameters<AmendOrderParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.trading_client(&ctx)?;
        let ticks = match (p.price_ticks, p.price.as_deref()) {
            (Some(t), _) => Some(t),
            (None, Some(price)) => {
                let order = try_tool!(client.get_order(p.order_id.trim()).await);
                let contract = try_tool!(client.get_market(&order.ticker).await);
                Some(try_tool!(
                    PriceEncoding::for_contract(&contract).parse(price)
                ))
            }
            (None, None) => None,
        };
        ok(try_tool!(
            client
                .amend_order(
                    p.order_id.trim(),
                    ticks,
                    p.count,
                    p.allow_cancel_replace.unwrap_or(true),
                    None,
                )
                .await
        ))
    }

    #[tool(
        description = "Cancel up to 50 orders by id in one call. Returns which were cancelled and which failed.",
        annotations(
            title = "Cancel orders",
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn cancel_orders(
        &self,
        Parameters(p): Parameters<CancelOrdersParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.trading_client(&ctx)?;
        ok(try_tool!(client.cancel_batch(&p.order_ids, None).await))
    }

    #[tool(
        description = "Close all or part of a position with a reduce-only order (market IOC by default, a limit order when a price is given). Errors with NO_POSITION when flat.",
        annotations(
            title = "Close position",
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn close_position(
        &self,
        Parameters(p): Parameters<ClosePositionParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.trading_client(&ctx)?;
        let contract = try_tool!(client.get_market(p.ticker.trim()).await);
        let limit = match (p.price_ticks, p.price.as_deref()) {
            (Some(t), _) => Some(t),
            (None, Some(price)) => Some(try_tool!(
                PriceEncoding::for_contract(&contract).parse(price)
            )),
            (None, None) => None,
        };
        ok(try_tool!(
            client
                .close_position(&contract.ticker, p.count, limit, None)
                .await
        ))
    }

    #[tool(
        description = "Dead man's switch: arm it (1-3600 seconds) and every working order is cancelled automatically if the deadline passes without a refresh; placing an order or calling this again pushes the deadline out. 0 disarms. Use it while running unattended.",
        annotations(
            title = "Dead man's switch",
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn set_cancel_after(
        &self,
        Parameters(p): Parameters<CancelAfterParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.trading_client(&ctx)?;
        ok(try_tool!(client.set_cancel_after(p.seconds, None).await))
    }

    #[tool(
        description = "The SarvaEX trading rules and conventions every agent must follow (price encodings, idempotency, how to read order responses). Read this once per session.",
        annotations(title = "Trading rules", read_only_hint = true)
    )]
    async fn get_trading_rules(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(SKILL_MD)]))
    }
}

pub const INSTRUCTIONS: &str = "SarvaEX is a prediction-market exchange with binary YES/NO contracts (prices in cents, 51 = $0.51 = 51%) and numeric futures (LONG/SHORT, prices in contract-specific ticks). \
Before trading: call get_market and get_orderbook for the ticker. Only OPEN contracts accept orders. Sides must match the kind (YES/NO vs LONG/SHORT). \
place_order returns the authoritative status: a 'rejected' result or status other than FILLED means the user does not have that position yet. \
Call place_order with dry_run=true first unless the user has already confirmed exact size and price. Quote prices in display units (e.g. $0.51, 4.30%) and money in USDC. \
Never retry an order blindly; check list_orders / list_positions first. Read get_trading_rules for the full conventions.";

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SarvaexMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(
            Implementation::new("sarvaex", env!("CARGO_PKG_VERSION")).with_title(format!(
                "SarvaEX Exchange ({})",
                self.state.environment_label
            )),
        )
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let res = Resource::new(RULES_URI, "SarvaEX trading rules")
            .with_title("SarvaEX trading rules")
            .with_description(
                "Conventions for prices, idempotency, order status handling and limits on SarvaEX.",
            )
            .with_mime_type("text/markdown");
        Ok(ListResourcesResult::with_all_items(vec![res]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri == RULES_URI {
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                SKILL_MD,
                request.uri,
            )])
            .into());
        }
        Err(McpError::resource_not_found(
            format!("unknown resource {}", request.uri),
            None,
        ))
    }
}
