//! Data models for the SarvaEX public REST and WebSocket API.
//!
//! Field names mirror the JSON emitted by `gw-rest` exactly. Every numeric
//! quantity on the exchange is an integer: prices are `price_ticks`, sizes are
//! `count`, money is micro-USDC. Nothing here uses floating point.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Contract kind as returned by the API (`kind` is a protobuf enum integer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum ContractKind {
    Binary,
    /// Numeric futures / scalar contract traded LONG/SHORT.
    Futures,
    Unknown,
}

impl ContractKind {
    pub fn from_code(code: i64) -> Self {
        match code {
            1 => Self::Binary,
            2 => Self::Futures,
            _ => Self::Unknown,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "BINARY",
            Self::Futures => "FUTURES",
            Self::Unknown => "UNKNOWN",
        }
    }
    /// The API query-parameter value for `kind=`.
    pub fn api_filter(self) -> Option<&'static str> {
        match self {
            Self::Binary => Some("BINARY"),
            Self::Futures => Some("FUTURES"),
            Self::Unknown => None,
        }
    }
}

impl std::str::FromStr for ContractKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "BINARY" | "BIN" | "YESNO" => Ok(Self::Binary),
            "FUTURES" | "FUTURE" | "SCALAR" | "FUT" => Ok(Self::Futures),
            other => Err(format!(
                "unknown contract kind '{other}' (use binary or futures)"
            )),
        }
    }
}

/// Contract lifecycle state (`state` is a protobuf enum integer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum ContractState {
    Draft,
    Listed,
    Open,
    Closed,
    Resolving,
    Settled,
    Cancelled,
    Halted,
    Unknown,
}

impl ContractState {
    pub fn from_code(code: i64) -> Self {
        match code {
            1 => Self::Draft,
            2 => Self::Listed,
            3 => Self::Open,
            4 => Self::Closed,
            5 => Self::Resolving,
            6 => Self::Settled,
            7 => Self::Cancelled,
            8 => Self::Halted,
            _ => Self::Unknown,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "DRAFT",
            Self::Listed => "LISTED",
            Self::Open => "OPEN",
            Self::Closed => "CLOSED",
            Self::Resolving => "RESOLVING",
            Self::Settled => "SETTLED",
            Self::Cancelled => "CANCELLED",
            Self::Halted => "HALTED",
            Self::Unknown => "UNKNOWN",
        }
    }
}

impl std::str::FromStr for ContractState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "DRAFT" => Ok(Self::Draft),
            "LISTED" => Ok(Self::Listed),
            "OPEN" => Ok(Self::Open),
            "CLOSED" => Ok(Self::Closed),
            "RESOLVING" => Ok(Self::Resolving),
            "SETTLED" => Ok(Self::Settled),
            "CANCELLED" | "CANCELED" => Ok(Self::Cancelled),
            "HALTED" => Ok(Self::Halted),
            other => Err(format!("unknown market state '{other}'")),
        }
    }
}

/// Order side. Binary contracts use YES/NO, futures use LONG/SHORT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Yes,
    No,
    Long,
    Short,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "YES",
            Self::No => "NO",
            Self::Long => "LONG",
            Self::Short => "SHORT",
        }
    }
    pub fn is_binary(self) -> bool {
        matches!(self, Self::Yes | Self::No)
    }
    pub fn matches_kind(self, kind: ContractKind) -> bool {
        match kind {
            ContractKind::Binary => self.is_binary(),
            ContractKind::Futures => !self.is_binary(),
            ContractKind::Unknown => true,
        }
    }
}

impl std::str::FromStr for Side {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "YES" | "Y" => Ok(Self::Yes),
            "NO" | "N" => Ok(Self::No),
            "LONG" | "L" => Ok(Self::Long),
            "SHORT" | "S" => Ok(Self::Short),
            other => Err(format!(
                "unknown side '{other}' (binary: yes|no, futures: long|short)"
            )),
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum Action {
    Buy,
    Sell,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "BUY",
            Self::Sell => "SELL",
        }
    }
}

impl std::str::FromStr for Action {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "BUY" | "B" => Ok(Self::Buy),
            "SELL" | "S" => Ok(Self::Sell),
            other => Err(format!("unknown action '{other}' (buy|sell)")),
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderType {
    Limit,
    Market,
    /// Futures only: market order when the last price falls (selling a long) or rises (buying back a short) to the trigger.
    #[serde(rename = "STOP_MARKET")]
    StopMarket,
    /// Futures only: like `StopMarket`, but places a limit order at `price_ticks`.
    #[serde(rename = "STOP_LIMIT")]
    StopLimit,
    /// Futures only: market order when the last price moves in your favour to the trigger.
    #[serde(rename = "TAKE_PROFIT")]
    TakeProfit,
    /// Futures only: like `TakeProfit`, but places a limit order at `price_ticks`.
    #[serde(rename = "TAKE_PROFIT_LIMIT")]
    TakeProfitLimit,
}

impl OrderType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Limit => "LIMIT",
            Self::Market => "MARKET",
            Self::StopMarket => "STOP_MARKET",
            Self::StopLimit => "STOP_LIMIT",
            Self::TakeProfit => "TAKE_PROFIT",
            Self::TakeProfitLimit => "TAKE_PROFIT_LIMIT",
        }
    }

    /// A stop or take-profit: armed by a trigger price, placed only when the market reaches it.
    pub fn is_trigger(self) -> bool {
        !matches!(self, Self::Limit | Self::Market)
    }

    /// Orders that carry no price of their own and execute at the market when they run.
    pub fn is_market_style(self) -> bool {
        matches!(self, Self::Market | Self::StopMarket | Self::TakeProfit)
    }

    /// Orders that need a limit price (`price_ticks`).
    pub fn has_limit_price(self) -> bool {
        matches!(self, Self::Limit | Self::StopLimit | Self::TakeProfitLimit)
    }
}

impl std::str::FromStr for OrderType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normal = s.trim().to_ascii_uppercase().replace(['-', ' '], "_");
        match normal.as_str() {
            "LIMIT" | "LMT" => Ok(Self::Limit),
            "MARKET" | "MKT" => Ok(Self::Market),
            "STOP_MARKET" | "STOP" => Ok(Self::StopMarket),
            "STOP_LIMIT" => Ok(Self::StopLimit),
            "TAKE_PROFIT" | "TAKE_PROFIT_MARKET" => Ok(Self::TakeProfit),
            "TAKE_PROFIT_LIMIT" => Ok(Self::TakeProfitLimit),
            other => Err(format!(
                "unknown order type '{other}' (limit, market, stop-market, stop-limit, take-profit, take-profit-limit)"
            )),
        }
    }
}

/// Time in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum Tif {
    Gtc,
    Ioc,
    Fok,
}

impl Tif {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gtc => "GTC",
            Self::Ioc => "IOC",
            Self::Fok => "FOK",
        }
    }
}

impl std::str::FromStr for Tif {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "GTC" => Ok(Self::Gtc),
            "IOC" => Ok(Self::Ioc),
            "FOK" => Ok(Self::Fok),
            other => Err(format!("unknown time-in-force '{other}' (GTC|IOC|FOK)")),
        }
    }
}

/// Self-trade prevention mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Stp {
    Unspecified,
    TakerAtCross,
    Maker,
}

impl Stp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unspecified => "UNSPECIFIED",
            Self::TakerAtCross => "TAKER_AT_CROSS",
            Self::Maker => "MAKER",
        }
    }
}

impl std::str::FromStr for Stp {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().replace('-', "_").as_str() {
            "UNSPECIFIED" | "NONE" => Ok(Self::Unspecified),
            "TAKER_AT_CROSS" => Ok(Self::TakerAtCross),
            "MAKER" => Ok(Self::Maker),
            other => Err(format!(
                "unknown stp mode '{other}' (unspecified|taker_at_cross|maker)"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Reference data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Contract {
    pub ticker: String,
    pub event_ticker: String,
    pub series_ticker: String,
    /// Protobuf enum integer: 1 = binary, 2 = futures/scalar. Use [`Contract::kind`].
    pub kind: i64,
    pub question: String,
    pub underlying: String,
    pub tick_size: i64,
    pub min_price_ticks: i64,
    pub max_price_ticks: i64,
    pub lower_bound_ticks: i64,
    pub upper_bound_ticks: i64,
    pub multiplier_micro_usdc: i64,
    pub divider: i64,
    pub multiplier_micro_per_display_unit: i64,
    pub tick_value_micro: i64,
    pub max_order_size: i64,
    pub position_limit_per_user: i64,
    /// Protobuf enum integer. Use [`Contract::state`].
    pub state: i64,
    pub listed_at: Option<String>,
    pub open_at: Option<String>,
    pub close_at: Option<String>,
    pub expected_resolution_at: Option<String>,
    pub settlement_source: String,
    pub oracle_policy: String,
    pub settlement_rule: Option<Value>,
    pub close_global_seq: i64,
    pub payoff_rule: Option<Value>,
    pub payout_ticks: Option<i64>,
}

impl Contract {
    pub fn kind(&self) -> ContractKind {
        ContractKind::from_code(self.kind)
    }
    pub fn state(&self) -> ContractState {
        ContractState::from_code(self.state)
    }
    /// True once the close time (or, without one, the expected resolution time) has passed.
    /// An expired contract can still say `OPEN` while it waits to be settled.
    pub fn is_expired_at(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        let parse = |value: &Option<String>| {
            value
                .as_deref()
                .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                .map(|time| time.with_timezone(&chrono::Utc))
        };
        parse(&self.close_at)
            .or_else(|| parse(&self.expected_resolution_at))
            .is_some_and(|deadline| deadline <= now)
    }
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(chrono::Utc::now())
    }
    pub fn is_open(&self) -> bool {
        self.state() == ContractState::Open
    }
    /// Category label from `settlement_rule.category`, if present.
    pub fn category(&self) -> Option<&str> {
        self.settlement_rule
            .as_ref()
            .and_then(|r| r.get("category"))
            .and_then(Value::as_str)
    }
}

/// One page of a cursor-paginated list. The gateway names the array differently
/// per endpoint (`contracts`, `orders`, `fills`, ...); all of them land in `items`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Page<T> {
    #[serde(default)]
    #[serde(
        alias = "contracts",
        alias = "series",
        alias = "events",
        alias = "fills"
    )]
    #[serde(
        alias = "orders",
        alias = "positions",
        alias = "entries",
        alias = "quotes"
    )]
    #[serde(alias = "api_keys", alias = "requests")]
    pub items: Vec<T>,
    #[serde(default)]
    pub next_cursor: String,
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            next_cursor: String::new(),
        }
    }
}

impl<T> Page<T> {
    pub fn has_more(&self) -> bool {
        !self.next_cursor.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Series {
    pub series_ticker: String,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Event {
    pub event_ticker: String,
    pub series_ticker: String,
    pub title: String,
    pub description: String,
    pub expected_resolution_at: Option<String>,
    pub margin_model: Option<Value>,
    pub outcome_kind: Option<Value>,
    pub trading_halted: Option<bool>,
    pub outcome_grid: Option<Value>,
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default)]
pub struct BookLevel {
    pub price_ticks: i64,
    pub total_qty: i64,
    pub order_count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct OrderBook {
    pub ticker: String,
    pub seq: i64,
    pub ts: Option<String>,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    /// Binary contracts only: the YES bids, best first.
    pub yes: Vec<BookLevel>,
    /// Binary contracts only: the NO bids, best first (a NO bid at q is a YES ask at 100 - q).
    pub no: Vec<BookLevel>,
    /// Binary contracts only: best price on each side, as the gateway derives it.
    pub best: Option<BookBest>,
}

/// Best prices on both outcomes of a binary contract.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default)]
pub struct BookBest {
    pub yes_bid: Option<i64>,
    pub yes_ask: Option<i64>,
    pub no_bid: Option<i64>,
    pub no_ask: Option<i64>,
}

impl OrderBook {
    /// Best prices on both outcomes. Uses the gateway's `best` when present, otherwise derives
    /// them from the YES book: buying NO costs `100 - best YES bid`, selling it gets
    /// `100 - best YES ask`.
    pub fn outcome_best(&self) -> BookBest {
        if let Some(best) = &self.best {
            return best.clone();
        }
        let yes_bid = self.best_bid().map(|l| l.price_ticks);
        let yes_ask = self.best_ask().map(|l| l.price_ticks);
        BookBest {
            yes_bid,
            yes_ask,
            no_bid: yes_ask.map(|p| 100 - p),
            no_ask: yes_bid.map(|p| 100 - p),
        }
    }

    pub fn best_bid(&self) -> Option<&BookLevel> {
        self.bids.iter().max_by_key(|l| l.price_ticks)
    }
    pub fn best_ask(&self) -> Option<&BookLevel> {
        self.asks.iter().min_by_key(|l| l.price_ticks)
    }
    /// Mid price in ticks*2 to stay integer (i.e. bid+ask). `None` without both sides.
    pub fn bid_plus_ask(&self) -> Option<i64> {
        Some(self.best_bid()?.price_ticks + self.best_ask()?.price_ticks)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct PublicFill {
    pub fill_id: String,
    pub ticker: String,
    pub global_seq: i64,
    pub contract_seq: i64,
    pub maker_side: String,
    pub maker_action: String,
    pub taker_side: String,
    pub taker_action: String,
    pub price_ticks: i64,
    pub count: i64,
    pub aggressor_side: Value,
    pub maker_fee_micro_usdc: i64,
    pub taker_fee_micro_usdc: i64,
    /// Binary contracts: the trade price as a YES and as a NO price (they add up to 100).
    pub yes_price_ticks: Option<i64>,
    pub no_price_ticks: Option<i64>,
    pub ts: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct OpenInterest {
    pub ticker: String,
    pub total_open_long: i64,
    pub total_open_short: i64,
    pub as_of_global_seq: i64,
    pub as_of: Option<String>,
}

// ---------------------------------------------------------------------------
// Trading
// ---------------------------------------------------------------------------

/// A new order request. Build with [`NewOrder::limit`] / [`NewOrder::market`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewOrder {
    pub client_order_id: String,
    pub ticker: String,
    pub side: Side,
    pub action: Action,
    pub order_type: OrderType,
    /// Tick-aligned limit price; `0` for market orders (gateway derives protection price).
    pub price_ticks: i64,
    pub count: i64,
    pub tif: Tif,
    pub post_only: bool,
    pub reduce_only: bool,
    pub stp: Stp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Trigger orders only: the last trade price that arms the order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price_ticks: Option<i64>,
}

impl NewOrder {
    /// A futures stop or take-profit. `limit_price_ticks` is required for the `*_LIMIT`
    /// types and ignored for the market ones. Trigger orders reduce a position by default;
    /// set `reduce_only = false` to let one open or grow a position.
    pub fn trigger(
        ticker: impl Into<String>,
        side: Side,
        action: Action,
        order_type: OrderType,
        trigger_price_ticks: i64,
        limit_price_ticks: Option<i64>,
        count: i64,
    ) -> Self {
        Self {
            client_order_id: crate::idempotency::client_order_id(),
            ticker: ticker.into(),
            side,
            action,
            order_type,
            price_ticks: if order_type.has_limit_price() {
                limit_price_ticks.unwrap_or(0)
            } else {
                0
            },
            count,
            tif: Tif::Gtc,
            post_only: false,
            reduce_only: true,
            stp: Stp::Unspecified,
            expires_at: None,
            trigger_price_ticks: Some(trigger_price_ticks),
        }
    }

    pub fn limit(
        ticker: impl Into<String>,
        side: Side,
        action: Action,
        price_ticks: i64,
        count: i64,
    ) -> Self {
        Self {
            client_order_id: crate::idempotency::client_order_id(),
            ticker: ticker.into(),
            side,
            action,
            order_type: OrderType::Limit,
            price_ticks,
            count,
            tif: Tif::Gtc,
            post_only: false,
            reduce_only: false,
            stp: Stp::Unspecified,
            expires_at: None,
            trigger_price_ticks: None,
        }
    }
    pub fn market(ticker: impl Into<String>, side: Side, action: Action, count: i64) -> Self {
        Self {
            order_type: OrderType::Market,
            tif: Tif::Ioc,
            ..Self::limit(ticker, side, action, 0, count)
        }
    }
    pub fn with_client_order_id(mut self, id: impl Into<String>) -> Self {
        self.client_order_id = id.into();
        self
    }
    pub fn with_tif(mut self, tif: Tif) -> Self {
        self.tif = tif;
        self
    }
    pub fn post_only(mut self, v: bool) -> Self {
        self.post_only = v;
        self
    }
    pub fn reduce_only(mut self, v: bool) -> Self {
        self.reduce_only = v;
        self
    }
    pub fn with_stp(mut self, stp: Stp) -> Self {
        self.stp = stp;
        self
    }
    pub fn expires_at(mut self, ts: Option<String>) -> Self {
        self.expires_at = ts;
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Order {
    pub order_id: String,
    pub client_order_id: String,
    pub user_id: String,
    pub ticker: String,
    pub side: String,
    pub action: String,
    pub price_ticks: i64,
    pub count: i64,
    pub filled_count: i64,
    pub remaining_count: i64,
    pub cancelled_count: i64,
    pub expired_count: i64,
    pub tif: String,
    pub post_only: bool,
    pub reduce_only: bool,
    pub stp: String,
    pub status: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub expires_at: Option<String>,
    pub hold_id: String,
    pub avg_fill_price_ticks: i64,
    /// Trigger orders only: STOP_MARKET, STOP_LIMIT, TAKE_PROFIT or TAKE_PROFIT_LIMIT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_type: Option<String>,
    /// Trigger orders only: the price that arms the order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price_ticks: Option<i64>,
    /// Trigger orders only: the order placed when it fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_order_id: Option<String>,
    /// Trigger orders only: why a trigger was rejected, cancelled or expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_code: Option<String>,
}

impl Order {
    /// True for a stop or take-profit (status `TRIGGER_*`).
    pub fn is_trigger(&self) -> bool {
        self.status.starts_with("TRIGGER_")
    }

    /// True when the order can no longer fill or fire (FILLED, CANCELLED, REJECTED, EXPIRED,
    /// or a trigger that fired, was cancelled, rejected or expired).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "FILLED"
                | "CANCELLED"
                | "REJECTED"
                | "EXPIRED"
                | "TRIGGER_FIRED"
                | "TRIGGER_CANCELLED"
                | "TRIGGER_REJECTED"
                | "TRIGGER_EXPIRED"
        )
    }
    pub fn is_working(&self) -> bool {
        matches!(self.status.as_str(), "OPEN" | "PARTIAL" | "PENDING")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct OrderFill {
    pub fill_id: String,
    pub order_id: String,
    pub ticker: String,
    pub price_ticks: i64,
    pub count: i64,
    pub aggressor_side: Value,
    pub fee_micro_usdc: i64,
    pub ts: Option<String>,
    pub seq: i64,
}

/// Response to order submission or cancellation.
///
/// HTTP 200 does **not** mean the order filled: inspect `order.status`,
/// `filled_count`, `remaining_count`, and `reject_code`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct OrderResponse {
    pub order: Order,
    pub fills: Vec<OrderFill>,
    pub reject_code: String,
    pub reject_reason: String,
}

impl OrderResponse {
    pub fn is_rejected(&self) -> bool {
        !self.reject_code.is_empty() || self.order.status == "REJECTED"
    }
    /// One-line human summary, e.g. `FILLED 10/10 @ 51 ticks`.
    pub fn summary(&self) -> String {
        if self.is_rejected() {
            let code = if self.reject_code.is_empty() {
                "REJECTED".to_owned()
            } else {
                self.reject_code.clone()
            };
            if self.reject_reason.is_empty() {
                return format!("REJECTED ({code})");
            }
            return format!("REJECTED ({code}): {}", self.reject_reason);
        }
        let o = &self.order;
        if o.is_trigger() {
            let mut s = format!("{} {} contract(s)", o.status, o.count);
            if let Some(t) = o.trigger_price_ticks {
                s.push_str(&format!(", arms at {t} ticks"));
            }
            if let Some(id) = o.fired_order_id.as_deref() {
                s.push_str(&format!(", fired as {id}"));
            }
            return s;
        }
        let mut s = format!("{} {}/{}", o.status, o.filled_count, o.count);
        if o.filled_count > 0 {
            s.push_str(&format!(" @ avg {} ticks", o.avg_fill_price_ticks));
        }
        if o.remaining_count > 0 {
            s.push_str(&format!(" ({} resting)", o.remaining_count));
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Account
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Balance {
    pub user_id: String,
    pub cash_micro_usdc: i64,
    pub held_micro_usdc: i64,
    pub total_micro_usdc: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RiskLimits {
    pub user_id: String,
    pub kyc_tier: i64,
    pub max_order_size_micro_usdc: i64,
    pub daily_loss_limit_micro_usdc: i64,
    pub per_contract_position_limit: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct AccountRisk {
    pub user_id: String,
    pub cash_micro_usdc: i64,
    pub held_micro_usdc: i64,
    pub total_micro_usdc: i64,
    pub available_collateral_micro_usdc: i64,
    pub equity_micro_usdc: i64,
    pub position_cost_micro_usdc: i64,
    pub realized_pnl_micro_usdc: i64,
    pub unrealized_pnl_micro_usdc: i64,
    pub open_positions: i64,
    pub open_orders: i64,
    pub as_of_global_seq: i64,
    pub limits: Option<RiskLimits>,
    pub calculation_basis: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct MarginPreview {
    pub event_ticker: String,
    pub eligible: bool,
    pub ineligible_reason: String,
    pub margin_before_micro_usdc: i64,
    pub margin_after_micro_usdc: i64,
    pub delta_micro_usdc: i64,
    pub standalone_delta_micro_usdc: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct LedgerEntry {
    pub tx_id: String,
    pub account_code: String,
    pub direction: String,
    pub amount_micro_usdc: i64,
    pub running_balance_micro_usdc: i64,
    pub reason_code: String,
    pub posted_at: Option<String>,
    pub memo: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct PrivateFill {
    pub fill_id: String,
    pub ticker: String,
    pub global_seq: i64,
    pub contract_seq: i64,
    pub order_id: String,
    pub role: String,
    pub side: String,
    pub action: String,
    pub price_ticks: i64,
    pub count: i64,
    pub aggressor_side: Value,
    pub fee_micro_usdc: i64,
    /// Binary contracts: the price on the side the order was stored on. A NO order is stored as
    /// the opposite YES order, so for NO traders `no_price_ticks` is the price they paid.
    pub outcome_price_ticks: Option<i64>,
    pub yes_price_ticks: Option<i64>,
    pub no_price_ticks: Option<i64>,
    pub ts: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Position {
    pub user_id: String,
    pub ticker: String,
    pub net_qty: i64,
    pub avg_cost_micro_usdc: i64,
    pub realized_pnl_micro_usdc: i64,
    pub unrealized_pnl_micro_usdc: i64,
    pub updated_at: Option<String>,
    pub last_global_seq: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Profile {
    pub user_id: String,
    pub name: String,
    pub email: String,
    pub status: String,
    pub created_at: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct LoginResponse {
    pub token: String,
    pub token_type: String,
    pub name: String,
    pub user_id: String,
    pub email: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ApiKeyInfo {
    pub key_id: String,
    pub key_prefix: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// Returned once at key creation; `key` is the only time the secret is visible.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ApiKeyCreated {
    pub key_id: String,
    pub name: String,
    pub key: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    pub created_at: Option<String>,
    pub expires_at: Option<String>,
}

/// All scopes the gateway accepts for API keys.
pub const ALL_SCOPES: [&str; 6] = [
    "markets:read",
    "account:read",
    "orders:read",
    "fills:read",
    "trading:write",
    "websocket:read",
];

pub const READ_SCOPES: [&str; 4] = ["markets:read", "account:read", "orders:read", "fills:read"];

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct DemoCreditResponse {
    pub ok: bool,
    pub balance: Balance,
}

// ---------------------------------------------------------------------------
// RFQ
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Rfq {
    pub rfq_id: String,
    pub client_rfq_id: String,
    pub creator_user_id: String,
    pub ticker: String,
    pub side: String,
    pub action: String,
    pub requested_count: i64,
    pub expires_at: Option<String>,
    pub status: String,
    pub accepted_quote_id: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Quote {
    pub quote_id: String,
    pub rfq_id: String,
    pub maker_user_id: String,
    pub bid_price_ticks: i64,
    pub offer_price_ticks: i64,
    pub available_count: i64,
    pub expires_at: Option<String>,
    pub status: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewRfq {
    pub client_rfq_id: String,
    pub ticker: String,
    pub side: Side,
    pub action: Action,
    pub requested_count: i64,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewQuote {
    pub quote_id: String,
    pub bid_price_ticks: i64,
    pub offer_price_ticks: i64,
    pub available_count: i64,
    pub expires_at: String,
}

// ---------------------------------------------------------------------------
// Market requests (user-proposed markets)
// ---------------------------------------------------------------------------

pub const MARKET_REQUEST_CATEGORIES: [&str; 6] = [
    "Economics",
    "Finance",
    "Crypto",
    "Commodities",
    "Climate",
    "Geopolitics / Shipping",
];

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewMarketRequest {
    /// 10–300 characters.
    pub question: String,
    /// `BINARY` or `FUTURE`.
    pub kind: String,
    /// One of [`MARKET_REQUEST_CATEGORIES`].
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MarketFilter {
    pub state: Option<ContractState>,
    pub kind: Option<ContractKind>,
    pub series_ticker: Option<String>,
    pub event_ticker: Option<String>,
    pub underlying: Option<String>,
    pub category: Option<String>,
    pub expected_resolution_from: Option<String>,
    pub expected_resolution_to: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl MarketFilter {
    pub fn open() -> Self {
        Self {
            state: Some(ContractState::Open),
            ..Default::default()
        }
    }
    pub(crate) fn to_query(&self) -> Vec<(&'static str, String)> {
        let mut q = Vec::new();
        if let Some(s) = self.state {
            q.push(("state", s.as_str().to_owned()));
        }
        if let Some(k) = self.kind.and_then(ContractKind::api_filter) {
            q.push(("kind", k.to_owned()));
        }
        if let Some(v) = &self.series_ticker {
            q.push(("series_ticker", v.clone()));
        }
        if let Some(v) = &self.event_ticker {
            q.push(("event_ticker", v.clone()));
        }
        if let Some(v) = &self.underlying {
            q.push(("underlying", v.clone()));
        }
        if let Some(v) = &self.category {
            q.push(("category", v.clone()));
        }
        if let Some(v) = &self.expected_resolution_from {
            q.push(("expected_resolution_from", v.clone()));
        }
        if let Some(v) = &self.expected_resolution_to {
            q.push(("expected_resolution_to", v.clone()));
        }
        if let Some(v) = self.limit {
            q.push(("limit", v.to_string()));
        }
        if let Some(v) = &self.cursor {
            q.push(("cursor", v.clone()));
        }
        q
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OrderFilter {
    pub ticker: Option<String>,
    pub status: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FillFilter {
    pub ticker: Option<String>,
    pub order_id: Option<String>,
    pub from_global_seq: Option<i64>,
    pub to_global_seq: Option<i64>,
    pub from_time: Option<String>,
    pub to_time: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

/// Events emitted by `gw-ws`. Unknown message types are preserved in `Other`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEvent {
    Connected {
        #[serde(default)]
        service: String,
    },
    Subscribed {
        #[serde(default)]
        channel: String,
        #[serde(default)]
        ticker: String,
    },
    MarketBookSnapshot {
        ticker: String,
        #[serde(default)]
        seq: i64,
        #[serde(default)]
        book_seq: i64,
        #[serde(default)]
        bids: Vec<BookLevel>,
        #[serde(default)]
        asks: Vec<BookLevel>,
    },
    MarketBookDelta {
        #[serde(default)]
        event_id: String,
        ticker: String,
        #[serde(default)]
        global_seq: i64,
        #[serde(default)]
        contract_seq: i64,
        #[serde(default)]
        book_seq: i64,
        #[serde(default)]
        book_side: String,
        #[serde(default)]
        side: Value,
        #[serde(default)]
        price_ticks: i64,
        #[serde(default)]
        qty_delta: i64,
        #[serde(default)]
        new_total_qty: i64,
    },
    MarketTrade {
        #[serde(default)]
        event_id: String,
        ticker: String,
        #[serde(default)]
        global_seq: i64,
        #[serde(default)]
        contract_seq: i64,
        #[serde(default)]
        price_ticks: i64,
        #[serde(default)]
        count: i64,
        #[serde(default)]
        aggressor_side: Value,
    },
    PrivateFill {
        #[serde(default)]
        event_id: String,
        ticker: String,
        #[serde(default)]
        global_seq: i64,
        #[serde(default)]
        contract_seq: i64,
        #[serde(default)]
        order_id: String,
        #[serde(default)]
        role: String,
        #[serde(default)]
        price_ticks: i64,
        #[serde(default)]
        count: i64,
        #[serde(default)]
        aggressor_side: Value,
    },
    Error {
        #[serde(default)]
        code: String,
        #[serde(default)]
        message: String,
    },
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod yes_no_tests {
    use super::*;

    fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn contract_is_expired_by_close_time_then_resolution_time() {
        let now = at("2026-11-06T00:00:00Z");
        let mut c = Contract::default();
        assert!(!c.is_expired_at(now), "no times means not expired");
        c.expected_resolution_at = Some("2026-10-01T00:00:00+00:00".into());
        assert!(c.is_expired_at(now), "past resolution with no close time");
        c.close_at = Some("2026-12-01T00:00:00+00:00".into());
        assert!(!c.is_expired_at(now), "a future close time wins");
        c.close_at = Some("2026-11-05T23:59:59+00:00".into());
        assert!(c.is_expired_at(now));
    }

    #[test]
    fn book_best_is_derived_from_the_yes_book_when_the_gateway_omits_it() {
        let level = |p| BookLevel {
            price_ticks: p,
            total_qty: 5,
            order_count: 1,
        };
        let book = OrderBook {
            bids: vec![level(42)],
            asks: vec![level(44)],
            ..Default::default()
        };
        let best = book.outcome_best();
        assert_eq!((best.yes_bid, best.yes_ask), (Some(42), Some(44)));
        // Buying NO costs 100 - best YES bid; a NO bid rests at 100 - best YES ask.
        assert_eq!((best.no_ask, best.no_bid), (Some(58), Some(56)));
    }

    #[test]
    fn book_and_fills_parse_the_yes_no_fields() {
        let book: OrderBook = serde_json::from_str(
            r#"{"ticker":"T","seq":1,"bids":[],"asks":[],
                "yes":[{"price_ticks":49,"total_qty":120,"order_count":8}],
                "no":[{"price_ticks":49,"total_qty":96,"order_count":6}],
                "best":{"yes_bid":49,"yes_ask":51,"no_bid":49,"no_ask":51}}"#,
        )
        .unwrap();
        assert_eq!(book.yes.len(), 1);
        assert_eq!(book.no[0].total_qty, 96);
        assert_eq!(book.outcome_best().no_ask, Some(51));

        let fill: PrivateFill = serde_json::from_str(
            r#"{"fill_id":"f","price_ticks":60,"outcome_price_ticks":60,"yes_price_ticks":60,"no_price_ticks":40}"#,
        )
        .unwrap();
        assert_eq!(fill.no_price_ticks, Some(40));
        let old: PublicFill = serde_json::from_str(r#"{"fill_id":"f","price_ticks":60}"#).unwrap();
        assert_eq!(old.no_price_ticks, None, "older gateways omit the fields");
    }
}

#[cfg(test)]
mod trigger_order_tests {
    use super::*;

    #[test]
    fn order_types_parse_in_every_spelling_and_serialize_with_underscores() {
        for (text, want) in [
            ("stop-market", OrderType::StopMarket),
            ("STOP_MARKET", OrderType::StopMarket),
            ("stop", OrderType::StopMarket),
            ("Stop Limit", OrderType::StopLimit),
            ("take-profit", OrderType::TakeProfit),
            ("take_profit_limit", OrderType::TakeProfitLimit),
            ("limit", OrderType::Limit),
            ("MKT", OrderType::Market),
        ] {
            assert_eq!(text.parse::<OrderType>().unwrap(), want, "{text}");
        }
        assert!("sideways".parse::<OrderType>().is_err());
        for t in [
            OrderType::Limit,
            OrderType::Market,
            OrderType::StopMarket,
            OrderType::StopLimit,
            OrderType::TakeProfit,
            OrderType::TakeProfitLimit,
        ] {
            assert_eq!(
                serde_json::to_value(t).unwrap(),
                t.as_str(),
                "wire name of {t:?}"
            );
            assert_eq!(
                serde_json::from_value::<OrderType>(serde_json::json!(t.as_str())).unwrap(),
                t
            );
        }
    }

    #[test]
    fn order_type_classification() {
        assert!(!OrderType::Limit.is_trigger() && !OrderType::Market.is_trigger());
        for t in [
            OrderType::StopMarket,
            OrderType::StopLimit,
            OrderType::TakeProfit,
            OrderType::TakeProfitLimit,
        ] {
            assert!(t.is_trigger());
        }
        assert!(
            OrderType::Market.is_market_style()
                && OrderType::StopMarket.is_market_style()
                && OrderType::TakeProfit.is_market_style()
        );
        assert!(!OrderType::StopLimit.is_market_style() && !OrderType::Limit.is_market_style());
        assert!(
            OrderType::StopLimit.has_limit_price()
                && OrderType::TakeProfitLimit.has_limit_price()
                && OrderType::Limit.has_limit_price()
        );
        assert!(!OrderType::StopMarket.has_limit_price() && !OrderType::Market.has_limit_price());
    }

    #[test]
    fn trigger_orders_reduce_only_by_default_and_ordinary_orders_omit_the_trigger_field() {
        let stop = NewOrder::trigger(
            "SXF-A",
            Side::Long,
            Action::Sell,
            OrderType::StopMarket,
            420,
            Some(999),
            2,
        );
        assert!(stop.reduce_only, "protects a position by default");
        assert_eq!(
            stop.price_ticks, 0,
            "a market-style trigger carries no limit price"
        );
        assert_eq!(
            serde_json::to_value(&stop).unwrap()["trigger_price_ticks"],
            420
        );
        let limit = NewOrder::trigger(
            "SXF-A",
            Side::Long,
            Action::Sell,
            OrderType::StopLimit,
            420,
            Some(415),
            2,
        );
        assert_eq!(limit.price_ticks, 415);
        let plain = NewOrder::limit("SX-A", Side::Yes, Action::Buy, 51, 1);
        assert!(
            serde_json::to_value(&plain)
                .unwrap()
                .get("trigger_price_ticks")
                .is_none()
        );
    }

    #[test]
    fn trigger_statuses_are_terminal_once_fired_cancelled_rejected_or_expired() {
        let status = |s: &str| {
            serde_json::from_value::<Order>(serde_json::json!({"order_id": "trg_1", "status": s}))
                .unwrap()
        };
        for done in [
            "TRIGGER_FIRED",
            "TRIGGER_CANCELLED",
            "TRIGGER_REJECTED",
            "TRIGGER_EXPIRED",
            "FILLED",
        ] {
            assert!(status(done).is_terminal(), "{done}");
        }
        for live in ["TRIGGER_PENDING", "OPEN", "PARTIAL"] {
            assert!(!status(live).is_terminal(), "{live}");
        }
        assert!(status("TRIGGER_PENDING").is_trigger() && !status("OPEN").is_trigger());
        // An order from an older gateway has none of the trigger fields.
        let o = status("OPEN");
        assert!(o.trigger_price_ticks.is_none() && o.fired_order_id.is_none());
        assert!(
            serde_json::to_value(&o)
                .unwrap()
                .get("trigger_price_ticks")
                .is_none()
        );
    }
}
