//! Async HTTP client for the SarvaEX REST gateway.

use crate::error::{ErrorEnvelope, Result, SarvaexError};
use crate::idempotency;
use crate::models::*;
use crate::price;
use reqwest::{Method, StatusCode, header};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

pub const DEFAULT_BASE_URL: &str = "https://api.sarvaex.com";
pub const DEFAULT_WS_URL: &str = "wss://api.sarvaex.com/ws";
pub const LOCAL_BASE_URL: &str = "http://localhost:18080";
pub const LOCAL_WS_URL: &str = "ws://localhost:18082/ws";

/// How the client authenticates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Credential {
    #[default]
    None,
    /// `X-API-Key: svx_live_...` — the right choice for bots and agents.
    ApiKey(String),
    /// `Authorization: Bearer <jwt-or-demo-token>` — browser-style session.
    Bearer(String),
}

impl Credential {
    pub fn is_some(&self) -> bool {
        !matches!(self, Self::None)
    }
    /// Detects the credential type from a raw secret.
    pub fn from_secret(secret: impl Into<String>) -> Self {
        let s: String = secret.into();
        let t = s.trim();
        if t.is_empty() {
            Self::None
        } else if t.starts_with("svx_") {
            Self::ApiKey(t.to_owned())
        } else {
            Self::Bearer(t.to_owned())
        }
    }
    /// Redacted form for logs: `svx_live_1234…`.
    pub fn redacted(&self) -> String {
        match self {
            Self::None => "(none)".into(),
            Self::ApiKey(k) => format!("{}…", k.chars().take(17).collect::<String>()),
            Self::Bearer(_) => "Bearer …".into(),
        }
    }
}

/// Local safety guards applied before any mutating request leaves the process.
/// These exist so an autonomous agent cannot exceed limits its operator set.
#[derive(Debug, Clone, Default)]
pub struct Guards {
    /// Refuse every mutating call.
    pub read_only: bool,
    /// Maximum estimated notional per order in micro-USDC.
    pub max_order_notional_micro: Option<i64>,
    /// Maximum `count` per order.
    pub max_order_count: Option<i64>,
    /// If set, orders are only allowed on these tickers.
    pub allowed_tickers: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub base_url: Url,
    pub ws_url: Url,
    pub credential: Credential,
    pub timeout: Duration,
    /// Retries for safe requests (GETs, and POSTs that carry an Idempotency-Key).
    pub max_retries: u32,
    pub user_agent: String,
    pub guards: Guards,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: Url::parse(DEFAULT_BASE_URL).expect("static url"),
            ws_url: Url::parse(DEFAULT_WS_URL).expect("static url"),
            credential: Credential::None,
            timeout: Duration::from_secs(30),
            max_retries: 4,
            user_agent: format!("sarvaex-sdk/{}", env!("CARGO_PKG_VERSION")),
            guards: Guards::default(),
        }
    }
}

impl ClientConfig {
    pub fn new(base_url: &str) -> Result<Self> {
        let base = normalize_base_url(base_url)?;
        let ws = derive_ws_url(&base)?;
        Ok(Self {
            base_url: base,
            ws_url: ws,
            ..Default::default()
        })
    }
    pub fn with_credential(mut self, c: Credential) -> Self {
        self.credential = c;
        self
    }
    pub fn with_ws_url(mut self, ws: &str) -> Result<Self> {
        self.ws_url = Url::parse(ws)
            .map_err(|e| SarvaexError::Validation(format!("invalid ws url '{ws}': {e}")))?;
        Ok(self)
    }
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
    pub fn with_guards(mut self, g: Guards) -> Self {
        self.guards = g;
        self
    }
    pub fn with_user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }
}

pub fn normalize_base_url(s: &str) -> Result<Url> {
    let mut s = s.trim().to_owned();
    if !s.contains("://") {
        s = format!("https://{s}");
    }
    if !s.ends_with('/') {
        s.push('/');
    }
    Url::parse(&s).map_err(|e| SarvaexError::Validation(format!("invalid base url '{s}': {e}")))
}

/// `https://api.sarvaex.com/` → `wss://api.sarvaex.com/ws`; local docker
/// (`:18080`) → `ws://host:18082/ws`.
pub fn derive_ws_url(base: &Url) -> Result<Url> {
    let mut ws = base.clone();
    let scheme = if base.scheme() == "http" { "ws" } else { "wss" };
    ws.set_scheme(scheme)
        .map_err(|_| SarvaexError::Validation("could not derive websocket url".into()))?;
    if base.port() == Some(18080) {
        let _ = ws.set_port(Some(18082));
    }
    ws.set_path("/ws");
    ws.set_query(None);
    Ok(ws)
}

/// Thread-safe, cheaply cloneable API client.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    cfg: Arc<ClientConfig>,
}

impl Client {
    pub fn new(cfg: ClientConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(Duration::from_secs(10))
            .user_agent(cfg.user_agent.clone())
            .build()?;
        Ok(Self {
            http,
            cfg: Arc::new(cfg),
        })
    }

    /// Convenience: production URL with the given credential.
    pub fn with_credential(base_url: &str, credential: Credential) -> Result<Self> {
        Self::new(ClientConfig::new(base_url)?.with_credential(credential))
    }

    pub fn config(&self) -> &ClientConfig {
        &self.cfg
    }
    pub fn base_url(&self) -> &Url {
        &self.cfg.base_url
    }
    pub fn ws_url(&self) -> &Url {
        &self.cfg.ws_url
    }
    pub fn credential(&self) -> &Credential {
        &self.cfg.credential
    }

    /// Same endpoint and settings, different credential (used by the remote MCP server
    /// to act on behalf of each connected user).
    pub fn with_swapped_credential(&self, credential: Credential) -> Self {
        let mut cfg = (*self.cfg).clone();
        cfg.credential = credential;
        Self {
            http: self.http.clone(),
            cfg: Arc::new(cfg),
        }
    }

    // -----------------------------------------------------------------------
    // Health
    // -----------------------------------------------------------------------

    pub async fn health_overview(&self) -> Result<Value> {
        self.get_json("v1/health/overview", &[]).await
    }
    pub async fn readyz(&self) -> Result<Value> {
        self.get_json("readyz", &[]).await
    }

    // -----------------------------------------------------------------------
    // Auth & account management
    // -----------------------------------------------------------------------

    pub async fn register(
        &self,
        name: &str,
        user_id: &str,
        email: &str,
        password: &str,
    ) -> Result<LoginResponse> {
        let body =
            json!({ "name": name, "user_id": user_id, "email": email, "password": password });
        self.send(
            Method::POST,
            "v1/auth/register",
            &[],
            Some(&body),
            None,
            false,
        )
        .await
    }

    /// Log in with an email **or** a user_id. Returns a short-lived bearer token.
    pub async fn login(&self, identifier: &str, password: &str) -> Result<LoginResponse> {
        let body = if identifier.contains('@') {
            json!({ "email": identifier, "password": password })
        } else {
            json!({ "user_id": identifier, "password": password })
        };
        self.send(Method::POST, "v1/auth/login", &[], Some(&body), None, false)
            .await
    }

    pub async fn profile(&self) -> Result<Profile> {
        self.get_auth("v1/account/profile", &[]).await
    }

    pub async fn list_api_keys(&self) -> Result<Vec<ApiKeyInfo>> {
        let page: Page<ApiKeyInfo> = self.get_auth("v1/account/api-keys", &[]).await?;
        Ok(page.items)
    }

    /// Create an API key. The returned `key` is shown exactly once.
    pub async fn create_api_key(
        &self,
        name: &str,
        scopes: &[&str],
        expires_at: Option<&str>,
    ) -> Result<ApiKeyCreated> {
        let mut body = json!({ "name": name, "scopes": scopes });
        if let Some(exp) = expires_at {
            body["expires_at"] = json!(exp);
        }
        self.send(
            Method::POST,
            "v1/account/api-keys",
            &[],
            Some(&body),
            None,
            true,
        )
        .await
    }

    pub async fn revoke_api_key(&self, key_id: &str) -> Result<Value> {
        self.send(
            Method::DELETE,
            &format!("v1/account/api-keys/{key_id}"),
            &[],
            None,
            None,
            true,
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Reference data (public)
    // -----------------------------------------------------------------------

    pub async fn list_markets(&self, filter: &MarketFilter) -> Result<Page<Contract>> {
        let q = filter.to_query();
        let q: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.get_json("v1/markets", &q).await
    }

    /// Follow `next_cursor` until exhausted (capped at `max_pages`).
    pub async fn list_all_markets(
        &self,
        filter: &MarketFilter,
        max_pages: usize,
    ) -> Result<Vec<Contract>> {
        let mut f = filter.clone();
        if f.limit.is_none() {
            f.limit = Some(500);
        }
        let mut out = Vec::new();
        for _ in 0..max_pages.max(1) {
            let page = self.list_markets(&f).await?;
            out.extend(page.items);
            if page.next_cursor.is_empty() {
                break;
            }
            f.cursor = Some(page.next_cursor);
        }
        Ok(out)
    }

    pub async fn get_market(&self, ticker: &str) -> Result<Contract> {
        self.get_json(&format!("v1/markets/{}", enc(ticker)), &[])
            .await
    }

    pub async fn list_series(
        &self,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<Page<Series>> {
        let limit_s = limit.map(|l| l.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(l) = &limit_s {
            q.push(("limit", l));
        }
        if let Some(c) = cursor {
            q.push(("cursor", c));
        }
        self.get_json("v1/series", &q).await
    }

    pub async fn list_events(
        &self,
        series_ticker: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<Page<Event>> {
        let limit_s = limit.map(|l| l.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(s) = series_ticker {
            q.push(("series_ticker", s));
        }
        if let Some(f) = from {
            q.push(("expected_resolution_from", f));
        }
        if let Some(t) = to {
            q.push(("expected_resolution_to", t));
        }
        if let Some(l) = &limit_s {
            q.push(("limit", l));
        }
        if let Some(c) = cursor {
            q.push(("cursor", c));
        }
        self.get_json("v1/events", &q).await
    }

    pub async fn get_event(&self, event_ticker: &str) -> Result<Event> {
        self.get_json(&format!("v1/events/{}", enc(event_ticker)), &[])
            .await
    }

    pub async fn get_resolution(&self, event_ticker: &str) -> Result<Value> {
        self.get_json(&format!("v1/events/{}/resolution", enc(event_ticker)), &[])
            .await
    }

    pub async fn get_settlement(&self, ticker: &str) -> Result<Value> {
        self.get_json(&format!("v1/markets/{}/settlement", enc(ticker)), &[])
            .await
    }

    // -----------------------------------------------------------------------
    // Market data (public)
    // -----------------------------------------------------------------------

    pub async fn orderbook(&self, ticker: &str, depth: Option<u32>) -> Result<OrderBook> {
        let depth_s = depth.map(|d| d.clamp(1, 100).to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(d) = &depth_s {
            q.push(("depth", d));
        }
        self.get_json(&format!("v1/markets/{}/orderbook", enc(ticker)), &q)
            .await
    }

    pub async fn market_fills(
        &self,
        ticker: &str,
        from_global_seq: Option<i64>,
        to_global_seq: Option<i64>,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<Page<PublicFill>> {
        let f = from_global_seq.map(|v| v.to_string());
        let t = to_global_seq.map(|v| v.to_string());
        let l = limit.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(v) = &f {
            q.push(("from_global_seq", v));
        }
        if let Some(v) = &t {
            q.push(("to_global_seq", v));
        }
        if let Some(v) = &l {
            q.push(("limit", v));
        }
        if let Some(c) = cursor {
            q.push(("cursor", c));
        }
        self.get_json(&format!("v1/markets/{}/fills", enc(ticker)), &q)
            .await
    }

    pub async fn open_interest(&self, ticker: &str) -> Result<OpenInterest> {
        self.get_json(&format!("v1/markets/{}/open-interest", enc(ticker)), &[])
            .await
    }

    // -----------------------------------------------------------------------
    // Trading
    // -----------------------------------------------------------------------

    /// Validate an order locally against the contract and the configured guards.
    /// Returns the contract so callers can display prices.
    pub async fn validate_order(&self, order: &NewOrder) -> Result<Contract> {
        let contract = self.get_market(&order.ticker).await?;
        if !contract.is_open() {
            return Err(SarvaexError::Validation(format!(
                "{} is {}, not OPEN; orders are rejected",
                contract.ticker,
                contract.state().as_str()
            )));
        }
        if !order.side.matches_kind(contract.kind()) {
            let expect = match contract.kind() {
                ContractKind::Binary => "YES or NO",
                ContractKind::Futures => "LONG or SHORT",
                ContractKind::Unknown => "a valid side",
            };
            return Err(SarvaexError::Validation(format!(
                "side {} is invalid for a {} contract; use {expect}",
                order.side,
                contract.kind().as_str()
            )));
        }
        if contract.is_expired() {
            return Err(SarvaexError::Validation(format!(
                "{} closed at {} and is waiting to be settled; do not trade it",
                contract.ticker,
                contract
                    .close_at
                    .as_deref()
                    .or(contract.expected_resolution_at.as_deref())
                    .unwrap_or("its expiry time")
            )));
        }
        price::validate_count(&contract, order.count)?;
        if order.order_type.is_trigger() {
            if contract.kind() != ContractKind::Futures {
                return Err(SarvaexError::Validation(format!(
                    "{} orders are for futures contracts; {} is a {} contract",
                    order.order_type.as_str(),
                    contract.ticker,
                    contract.kind().as_str()
                )));
            }
            let trigger = order.trigger_price_ticks.ok_or_else(|| {
                SarvaexError::Validation(
                    "a trigger order needs a trigger price (--trigger-price)".into(),
                )
            })?;
            price::validate_limit_price(&contract, trigger)
                .map_err(|e| SarvaexError::Validation(format!("trigger price: {e}")))?;
            if order.order_type.has_limit_price() {
                price::validate_limit_price(&contract, order.price_ticks)?;
            }
        } else {
            if order.trigger_price_ticks.is_some() {
                return Err(SarvaexError::Validation(
                    "a trigger price only applies to stop and take-profit orders (--type stop-market, stop-limit, take-profit or take-profit-limit)".into(),
                ));
            }
            if order.order_type == OrderType::Limit {
                price::validate_limit_price(&contract, order.price_ticks)?;
            }
        }
        if order.client_order_id.len() > 128 {
            return Err(SarvaexError::Validation(
                "client_order_id must be ≤ 128 characters".into(),
            ));
        }
        self.check_guards(&contract, order)?;
        Ok(contract)
    }

    fn check_guards(&self, contract: &Contract, order: &NewOrder) -> Result<()> {
        let g = &self.cfg.guards;
        if g.read_only {
            return Err(SarvaexError::Guard(
                "client is in read-only mode (SARVAEX_READ_ONLY)".into(),
            ));
        }
        if let Some(allow) = &g.allowed_tickers
            && !allow.iter().any(|t| t.eq_ignore_ascii_case(&order.ticker))
        {
            return Err(SarvaexError::Guard(format!(
                "ticker {} is not in SARVAEX_ALLOWED_TICKERS",
                order.ticker
            )));
        }
        if let Some(max) = g.max_order_count
            && order.count > max
        {
            return Err(SarvaexError::Guard(format!(
                "count {} exceeds SARVAEX_MAX_ORDER_COUNT={max}",
                order.count
            )));
        }
        if let Some(max) = g.max_order_notional_micro {
            let px = if order.order_type.is_market_style() {
                // Worst case for the guard: a binary fills at 99¢; futures at the contract max.
                match contract.kind() {
                    ContractKind::Binary => 99,
                    _ => contract.max_price_ticks.max(1),
                }
            } else {
                order.price_ticks
            };
            match price::estimated_notional_micro(contract, px, order.count) {
                Some(n) if n > max => {
                    return Err(SarvaexError::Guard(format!(
                        "estimated notional {} USDC exceeds SARVAEX_MAX_ORDER_NOTIONAL_USDC={}",
                        price::micro_to_usdc(n),
                        price::micro_to_usdc(max)
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn require_mutation(&self) -> Result<()> {
        if self.cfg.guards.read_only {
            return Err(SarvaexError::Guard(
                "client is in read-only mode (SARVAEX_READ_ONLY)".into(),
            ));
        }
        Ok(())
    }

    /// Submit an order. Runs [`Client::validate_order`] first, then POSTs with
    /// the given (or a fresh) `Idempotency-Key`. A rejected order is returned as
    /// `Ok(OrderResponse)` with `reject_code` set — check [`OrderResponse::is_rejected`].
    pub async fn place_order(
        &self,
        order: &NewOrder,
        idempotency_key: Option<&str>,
    ) -> Result<OrderResponse> {
        self.validate_order(order).await?;
        self.place_order_unchecked(order, idempotency_key).await
    }

    /// Submit without local validation (still applies guards). Useful when the
    /// caller has already fetched the contract.
    pub async fn place_order_unchecked(
        &self,
        order: &NewOrder,
        idempotency_key: Option<&str>,
    ) -> Result<OrderResponse> {
        self.require_mutation()?;
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("order"));
        let body = serde_json::to_value(order).map_err(|e| SarvaexError::Other(e.to_string()))?;
        self.send_order_like(Method::POST, "v1/orders", &body, &key)
            .await
    }

    pub async fn list_orders(&self, filter: &OrderFilter) -> Result<Page<Order>> {
        let l = filter.limit.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(v) = &filter.ticker {
            q.push(("ticker", v));
        }
        if let Some(v) = &filter.status {
            q.push(("status", v));
        }
        if let Some(v) = &l {
            q.push(("limit", v));
        }
        if let Some(v) = &filter.cursor {
            q.push(("cursor", v));
        }
        self.get_auth("v1/orders", &q).await
    }

    pub async fn get_order(&self, order_id: &str) -> Result<Order> {
        self.get_auth(&format!("v1/orders/{}", enc(order_id)), &[])
            .await
    }

    pub async fn cancel_order(
        &self,
        order_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<OrderResponse> {
        self.require_mutation()?;
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("cancel"));
        self.send_order_like(
            Method::POST,
            &format!("v1/orders/{}/cancel", enc(order_id)),
            &json!({}),
            &key,
        )
        .await
    }

    /// Cancel every working order (optionally only for one ticker). Returns
    /// `(cancelled, failed)` order ids.
    pub async fn cancel_all_orders(
        &self,
        ticker: Option<&str>,
    ) -> Result<(Vec<String>, Vec<(String, String)>)> {
        self.require_mutation()?;
        // Gateways with the API expansion cancel everything in one call; older
        // ones answer 404/405 and we fall back to listing and cancelling one by one.
        match self.cancel_all_server(ticker, None).await {
            Ok(v) => {
                let ids = |key: &str| -> Vec<String> {
                    v[key]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let failed = v["failed"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|f| {
                                let id = f["order_id"].as_str().unwrap_or("").to_owned();
                                let why = f["reject_reason"]
                                    .as_str()
                                    .filter(|r| !r.is_empty())
                                    .or_else(|| f["reject_code"].as_str())
                                    .unwrap_or("rejected")
                                    .to_owned();
                                (id, why)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                return Ok((ids("cancelled"), failed));
            }
            Err(e) if matches!(e.status(), Some(404 | 405)) => {}
            Err(e) => return Err(e),
        }
        let mut cancelled = Vec::new();
        let mut failed = Vec::new();
        for status in ["OPEN", "PARTIAL", "PENDING"] {
            let mut cursor: Option<String> = None;
            loop {
                let page = self
                    .list_orders(&OrderFilter {
                        ticker: ticker.map(str::to_owned),
                        status: Some(status.into()),
                        limit: Some(200),
                        cursor: cursor.clone(),
                    })
                    .await?;
                for o in &page.items {
                    match self.cancel_order(&o.order_id, None).await {
                        Ok(r) if !r.is_rejected() => cancelled.push(o.order_id.clone()),
                        Ok(r) => failed.push((o.order_id.clone(), r.summary())),
                        Err(e) => failed.push((o.order_id.clone(), e.to_string())),
                    }
                }
                if page.next_cursor.is_empty() || page.items.is_empty() {
                    break;
                }
                cursor = Some(page.next_cursor);
            }
        }
        Ok((cancelled, failed))
    }

    // -----------------------------------------------------------------------
    // Account & portfolio
    // -----------------------------------------------------------------------

    pub async fn balance(&self) -> Result<Balance> {
        self.get_auth("v1/account/balance", &[]).await
    }

    pub async fn account_risk(&self) -> Result<AccountRisk> {
        self.get_auth("v1/account/risk", &[]).await
    }

    pub async fn account_margin(&self) -> Result<Value> {
        self.get_auth("v1/account/margin", &[]).await
    }

    /// What-if margin impact of an order. Read-only; no Idempotency-Key needed.
    pub async fn preview_margin(
        &self,
        ticker: &str,
        side: Side,
        action: Action,
        price_ticks: i64,
        count: i64,
    ) -> Result<MarginPreview> {
        let body = json!({ "ticker": ticker, "side": side, "action": action, "price_ticks": price_ticks, "count": count });
        self.send(
            Method::POST,
            "v1/account/margin/preview",
            &[],
            Some(&body),
            None,
            true,
        )
        .await
    }

    pub async fn account_history(
        &self,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<Page<LedgerEntry>> {
        let l = limit.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(v) = &l {
            q.push(("limit", v));
        }
        if let Some(c) = cursor {
            q.push(("cursor", c));
        }
        self.get_auth("v1/account/history", &q).await
    }

    pub async fn account_fills(&self, filter: &FillFilter) -> Result<Page<PrivateFill>> {
        let f = filter.from_global_seq.map(|v| v.to_string());
        let t = filter.to_global_seq.map(|v| v.to_string());
        let l = filter.limit.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(v) = &filter.ticker {
            q.push(("ticker", v));
        }
        if let Some(v) = &filter.order_id {
            q.push(("order_id", v));
        }
        if let Some(v) = &f {
            q.push(("from_global_seq", v));
        }
        if let Some(v) = &t {
            q.push(("to_global_seq", v));
        }
        if let Some(v) = &filter.from_time {
            q.push(("from_time", v));
        }
        if let Some(v) = &filter.to_time {
            q.push(("to_time", v));
        }
        if let Some(v) = &l {
            q.push(("limit", v));
        }
        if let Some(v) = &filter.cursor {
            q.push(("cursor", v));
        }
        self.get_auth("v1/account/fills", &q).await
    }

    pub async fn positions(
        &self,
        include_closed: bool,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<Page<Position>> {
        let l = limit.map(|v| v.to_string());
        let inc = include_closed.to_string();
        let mut q: Vec<(&str, &str)> = vec![("include_closed", &inc)];
        if let Some(v) = &l {
            q.push(("limit", v));
        }
        if let Some(c) = cursor {
            q.push(("cursor", c));
        }
        self.get_auth("v1/positions", &q).await
    }

    pub async fn position(&self, ticker: &str) -> Result<Position> {
        self.get_auth(&format!("v1/positions/{}", enc(ticker)), &[])
            .await
    }

    /// Demo/devnet only: credit play money to the account.
    pub async fn demo_credit(
        &self,
        amount_micro_usdc: i64,
        note: Option<&str>,
        idempotency_key: Option<&str>,
    ) -> Result<DemoCreditResponse> {
        self.require_mutation()?;
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("deposit"));
        let body = json!({ "amount_micro_usdc": amount_micro_usdc, "note": note.unwrap_or("sarvaex-cli demo funding") });
        self.send(
            Method::POST,
            "v1/demo/deposits/credit",
            &[],
            Some(&body),
            Some(&key),
            true,
        )
        .await
    }

    // -----------------------------------------------------------------------
    // RFQ
    // -----------------------------------------------------------------------

    pub async fn create_rfq(&self, rfq: &NewRfq, idempotency_key: Option<&str>) -> Result<Rfq> {
        self.require_mutation()?;
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("rfq"));
        let body = serde_json::to_value(rfq).map_err(|e| SarvaexError::Other(e.to_string()))?;
        let v: Value = self
            .send(Method::POST, "v1/rfqs", &[], Some(&body), Some(&key), true)
            .await?;
        unwrap_field(v, "rfq")
    }

    pub async fn get_rfq(&self, rfq_id: &str) -> Result<Rfq> {
        self.get_auth(&format!("v1/rfqs/{}", enc(rfq_id)), &[])
            .await
    }

    pub async fn list_rfq_quotes(
        &self,
        rfq_id: &str,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<Page<Quote>> {
        let l = limit.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(v) = &l {
            q.push(("limit", v));
        }
        if let Some(c) = cursor {
            q.push(("cursor", c));
        }
        self.get_auth(&format!("v1/rfqs/{}/quotes", enc(rfq_id)), &q)
            .await
    }

    pub async fn submit_rfq_quote(
        &self,
        rfq_id: &str,
        quote: &NewQuote,
        idempotency_key: Option<&str>,
    ) -> Result<Quote> {
        self.require_mutation()?;
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("quote"));
        let body = serde_json::to_value(quote).map_err(|e| SarvaexError::Other(e.to_string()))?;
        let v: Value = self
            .send(
                Method::POST,
                &format!("v1/rfqs/{}/quotes", enc(rfq_id)),
                &[],
                Some(&body),
                Some(&key),
                true,
            )
            .await?;
        unwrap_field(v, "quote")
    }

    pub async fn accept_rfq_quote(
        &self,
        rfq_id: &str,
        quote_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        self.require_mutation()?;
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("rfq-accept"));
        self.send(
            Method::POST,
            &format!("v1/rfqs/{}/quotes/{}/accept", enc(rfq_id), enc(quote_id)),
            &[],
            Some(&json!({})),
            Some(&key),
            true,
        )
        .await
    }

    pub async fn cancel_rfq(&self, rfq_id: &str, idempotency_key: Option<&str>) -> Result<Rfq> {
        self.require_mutation()?;
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("rfq-cancel"));
        let v: Value = self
            .send(
                Method::POST,
                &format!("v1/rfqs/{}/cancel", enc(rfq_id)),
                &[],
                Some(&json!({})),
                Some(&key),
                true,
            )
            .await?;
        unwrap_field(v, "rfq")
    }

    pub async fn cancel_rfq_quote(
        &self,
        rfq_id: &str,
        quote_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<Quote> {
        self.require_mutation()?;
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("quote-cancel"));
        let v: Value = self
            .send(
                Method::POST,
                &format!("v1/rfqs/{}/quotes/{}/cancel", enc(rfq_id), enc(quote_id)),
                &[],
                Some(&json!({})),
                Some(&key),
                true,
            )
            .await?;
        unwrap_field(v, "quote")
    }

    // -----------------------------------------------------------------------
    // Market requests
    // -----------------------------------------------------------------------

    pub async fn list_market_requests(&self) -> Result<Value> {
        self.get_auth("v1/market-requests", &[]).await
    }

    pub async fn create_market_request(&self, req: &NewMarketRequest) -> Result<Value> {
        self.require_mutation()?;
        let body = serde_json::to_value(req).map_err(|e| SarvaexError::Other(e.to_string()))?;
        self.send(
            Method::POST,
            "v1/market-requests",
            &[],
            Some(&body),
            None,
            true,
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Exchange status and extra market data (public)
    // -----------------------------------------------------------------------

    /// Gateway clock: `{epoch_ms, server_time}`.
    pub async fn server_time(&self) -> Result<Value> {
        self.get_json("v1/time", &[]).await
    }

    /// Whether the exchange accepts orders right now (`mode` OPEN / CANCEL_ONLY / CLOSED).
    pub async fn exchange_status(&self) -> Result<Value> {
        self.get_json("v1/exchange/status", &[]).await
    }

    /// Contracts closing in the next 7 days plus the current exchange status.
    pub async fn exchange_schedule(&self) -> Result<Value> {
        self.get_json("v1/exchange/schedule", &[]).await
    }

    /// The fee schedule (taker fee and maker incentive, in basis points).
    pub async fn fee_schedule(&self) -> Result<Value> {
        self.get_json("v1/fees", &[]).await
    }

    /// Last price, 24h volume, high/low and best bid/ask for one market.
    pub async fn market_stats(&self, ticker: &str) -> Result<Value> {
        self.get_json(&format!("v1/markets/{}/stats", enc(ticker)), &[])
            .await
    }

    /// Statistics for several markets in one call.
    pub async fn markets_stats(&self, tickers: &[String]) -> Result<Value> {
        let joined = tickers.join(",");
        self.get_json("v1/markets/stats", &[("tickers", joined.as_str())])
            .await
    }

    /// Public trades for a market, newest first, optionally within `[from, to)`.
    pub async fn market_trades(
        &self,
        ticker: &str,
        from: Option<&str>,
        to: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Value> {
        let limit = limit.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(v) = from {
            q.push(("from_ts", v));
        }
        if let Some(v) = to {
            q.push(("to_ts", v));
        }
        if let Some(v) = limit.as_deref() {
            q.push(("limit", v));
        }
        self.get_json(&format!("v1/markets/{}/trades", enc(ticker)), &q)
            .await
    }

    /// OHLC candles. `interval` is one of 1m, 5m, 15m, 1h, 4h, 1d.
    pub async fn candles(
        &self,
        ticker: &str,
        interval: &str,
        from: Option<&str>,
        to: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Value> {
        let limit = limit.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = vec![("interval", interval)];
        if let Some(v) = from {
            q.push(("from", v));
        }
        if let Some(v) = to {
            q.push(("to", v));
        }
        if let Some(v) = limit.as_deref() {
            q.push(("limit", v));
        }
        self.get_json(&format!("v1/markets/{}/candles", enc(ticker)), &q)
            .await
    }

    /// Up to 20 order books in one call.
    pub async fn orderbooks(&self, tickers: &[String], depth: Option<u32>) -> Result<Value> {
        let joined = tickers.join(",");
        let depth = depth.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = vec![("tickers", joined.as_str())];
        if let Some(v) = depth.as_deref() {
            q.push(("depth", v));
        }
        self.get_json("v1/orderbooks", &q).await
    }

    // -----------------------------------------------------------------------
    // Trading: amend, batch, cancel in bulk, close a position
    // -----------------------------------------------------------------------

    /// Change an order's price and/or count. With `allow_cancel_replace` the
    /// gateway cancels and re-submits when it cannot amend in place (queue
    /// priority is lost); the answer says which happened in `amended_via`.
    pub async fn amend_order(
        &self,
        order_id: &str,
        price_ticks: Option<i64>,
        count: Option<i64>,
        allow_cancel_replace: bool,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        self.require_mutation()?;
        if price_ticks.is_none() && count.is_none() {
            return Err(SarvaexError::Validation(
                "amend needs a new price and/or a new count".into(),
            ));
        }
        let mut body = json!({ "allow_cancel_replace": allow_cancel_replace });
        if let Some(p) = price_ticks {
            body["price_ticks"] = json!(p);
        }
        if let Some(c) = count {
            body["count"] = json!(c);
        }
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("amend"));
        self.send(
            Method::POST,
            &format!("v1/orders/{}/amend", enc(order_id)),
            &[],
            Some(&body),
            Some(&key),
            true,
        )
        .await
    }

    /// Submit 1-20 orders in one request. Not atomic: the answer lists the
    /// outcome of each order in `results`, with `accepted` and `failed` counts.
    pub async fn batch_orders(
        &self,
        orders: &[NewOrder],
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        self.require_mutation()?;
        if orders.is_empty() || orders.len() > 20 {
            return Err(SarvaexError::Validation(
                "a batch holds 1 to 20 orders".into(),
            ));
        }
        let body = json!({ "orders": orders });
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("batch"));
        self.send(
            Method::POST,
            "v1/orders/batch",
            &[],
            Some(&body),
            Some(&key),
            true,
        )
        .await
    }

    /// Cancel up to 50 orders by id.
    pub async fn cancel_batch(
        &self,
        order_ids: &[String],
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        self.require_mutation()?;
        if order_ids.is_empty() || order_ids.len() > 50 {
            return Err(SarvaexError::Validation(
                "cancel-batch takes 1 to 50 order ids".into(),
            ));
        }
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("cancel-batch"));
        self.send(
            Method::POST,
            "v1/orders/cancel-batch",
            &[],
            Some(&json!({ "order_ids": order_ids })),
            Some(&key),
            true,
        )
        .await
    }

    /// Cancel every working order, optionally for one ticker (single request).
    pub async fn cancel_all_server(
        &self,
        ticker: Option<&str>,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        self.require_mutation()?;
        let mut body = json!({});
        if let Some(t) = ticker {
            body["ticker"] = json!(t);
        }
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("cancel-all"));
        self.send(
            Method::POST,
            "v1/orders/cancel-all",
            &[],
            Some(&body),
            Some(&key),
            true,
        )
        .await
    }

    /// Close (part of) a position with a reduce-only order: a market IOC by
    /// default, a limit order when `price_ticks` is given.
    pub async fn close_position(
        &self,
        ticker: &str,
        count: Option<i64>,
        price_ticks: Option<i64>,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        self.require_mutation()?;
        let mut body = json!({});
        if let Some(c) = count {
            body["count"] = json!(c);
        }
        if let Some(p) = price_ticks {
            body["price_ticks"] = json!(p);
        }
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("close"));
        self.send(
            Method::POST,
            &format!("v1/positions/{}/close", enc(ticker)),
            &[],
            Some(&body),
            Some(&key),
            true,
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Account: dead man's switch, fees, activity, settlements, key limits
    // -----------------------------------------------------------------------

    /// Current dead man's switch: `{armed, seconds, cancel_at, armed_at}`.
    pub async fn cancel_after_status(&self) -> Result<Value> {
        self.get_auth("v1/account/cancel-after", &[]).await
    }

    /// Arm the dead man's switch (1-3600 s): every working order is cancelled
    /// if the deadline passes without a refresh. `0` disarms it.
    pub async fn set_cancel_after(
        &self,
        seconds: u32,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        self.require_mutation()?;
        if seconds > 3600 {
            return Err(SarvaexError::Validation(
                "cancel-after is 0 (off) or 1 to 3600 seconds".into(),
            ));
        }
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| idempotency::idempotency_key("cancel-after"));
        self.send(
            Method::POST,
            "v1/account/cancel-after",
            &[],
            Some(&json!({ "seconds": seconds })),
            Some(&key),
            true,
        )
        .await
    }

    /// Your 30-day maker/taker volume and the fees you paid.
    pub async fn account_fees(&self) -> Result<Value> {
        self.get_auth("v1/account/fees", &[]).await
    }

    /// Daily maker incentives paid to you.
    pub async fn account_incentives(&self) -> Result<Value> {
        self.get_auth("v1/account/incentives", &[]).await
    }

    /// Payouts from settled markets.
    pub async fn account_settlements(
        &self,
        ticker: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Value> {
        let limit = limit.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(t) = ticker {
            q.push(("ticker", t));
        }
        if let Some(l) = limit.as_deref() {
            q.push(("limit", l));
        }
        self.get_auth("v1/account/settlements", &q).await
    }

    /// One newest-first feed of fills, order updates, settlements and cash movements.
    pub async fn account_activity(
        &self,
        types: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        ticker: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Value> {
        let limit = limit.map(|v| v.to_string());
        let mut q: Vec<(&str, &str)> = Vec::new();
        if let Some(v) = types {
            q.push(("types", v));
        }
        if let Some(v) = from {
            q.push(("from", v));
        }
        if let Some(v) = to {
            q.push(("to", v));
        }
        if let Some(v) = ticker {
            q.push(("ticker", v));
        }
        if let Some(v) = limit.as_deref() {
            q.push(("limit", v));
        }
        self.get_auth("v1/account/activity", &q).await
    }

    /// Create an API key with trading caps (`limits` is the gateway's limits object).
    pub async fn create_api_key_with_limits(
        &self,
        name: &str,
        scopes: &[&str],
        expires_at: Option<&str>,
        limits: &Value,
    ) -> Result<ApiKeyCreated> {
        let mut body = json!({ "name": name, "scopes": scopes, "limits": limits });
        if let Some(exp) = expires_at {
            body["expires_at"] = json!(exp);
        }
        self.send(
            Method::POST,
            "v1/account/api-keys",
            &[],
            Some(&body),
            None,
            true,
        )
        .await
    }

    /// Replace a key's limits; `None` removes them all.
    pub async fn set_api_key_limits(&self, key_id: &str, limits: Option<&Value>) -> Result<Value> {
        self.require_mutation()?;
        let body = json!({ "limits": limits.cloned().unwrap_or(Value::Null) });
        self.send(
            Method::PATCH,
            &format!("v1/account/api-keys/{}/limits", enc(key_id)),
            &[],
            Some(&body),
            None,
            true,
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Raw access (escape hatch for endpoints added after this SDK)
    // -----------------------------------------------------------------------

    /// Perform an arbitrary request against the gateway and return the JSON body.
    pub async fn raw(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
        idempotency_key: Option<&str>,
    ) -> Result<Value> {
        if method != Method::GET {
            self.require_mutation()?;
        }
        self.send(
            method,
            path.trim_start_matches('/'),
            query,
            body,
            idempotency_key,
            true,
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    /// Public endpoint: a credential is attached if configured but not required.
    async fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        self.send(Method::GET, path, query, None, None, false).await
    }

    /// Private endpoint: fails fast with `Unauthenticated` when no credential is configured.
    async fn get_auth<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        self.send(Method::GET, path, query, None, None, true).await
    }

    fn apply_auth(
        &self,
        req: reqwest::RequestBuilder,
        require: bool,
    ) -> Result<reqwest::RequestBuilder> {
        match &self.cfg.credential {
            Credential::ApiKey(k) => Ok(req.header("X-API-Key", k)),
            Credential::Bearer(t) => Ok(req.header(header::AUTHORIZATION, format!("Bearer {t}"))),
            Credential::None if require => Err(SarvaexError::Unauthenticated(
                "set SARVAEX_API_KEY or run `sarvaex auth login`".into(),
            )),
            Credential::None => Ok(req),
        }
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.cfg
            .base_url
            .join(path)
            .map_err(|e| SarvaexError::Validation(format!("bad path '{path}': {e}")))
    }

    /// Core request loop with bounded retries on transient failures.
    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
        idempotency_key: Option<&str>,
        require_auth: bool,
    ) -> Result<T> {
        let (status, retry_after, text) = self
            .send_text(method, path, query, body, idempotency_key, require_auth)
            .await?;
        if status.is_success() {
            return decode(path, &text);
        }
        Err(parse_error(status, &text, retry_after))
    }

    /// Order submit/cancel: non-2xx may still carry an `OrderResponse` body.
    async fn send_order_like(
        &self,
        method: Method,
        path: &str,
        body: &Value,
        idempotency_key: &str,
    ) -> Result<OrderResponse> {
        let (status, retry_after, text) = self
            .send_text(method, path, &[], Some(body), Some(idempotency_key), true)
            .await?;
        if let Ok(resp) = serde_json::from_str::<OrderResponse>(&text)
            && (!resp.reject_code.is_empty()
                || !resp.order.order_id.is_empty()
                || status.is_success())
        {
            return Ok(resp);
        }
        Err(parse_error(status, &text, retry_after))
    }

    async fn send_text(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
        idempotency_key: Option<&str>,
        require_auth: bool,
    ) -> Result<(StatusCode, Option<u64>, String)> {
        let mut url = self.url(path)?;
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in query {
                pairs.append_pair(k, v);
            }
        }
        let is_safe = method == Method::GET || idempotency_key.is_some();
        let max_attempts = if is_safe { self.cfg.max_retries + 1 } else { 1 };
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut req = self
                .http
                .request(method.clone(), url.clone())
                .header(header::ACCEPT, "application/json");
            req = self.apply_auth(req, require_auth)?;
            if let Some(k) = idempotency_key {
                req = req.header("Idempotency-Key", k);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            tracing::debug!(%method, %url, attempt, "sarvaex request");
            let outcome: std::result::Result<(StatusCode, Option<u64>, String), reqwest::Error> =
                async {
                    let resp = req.send().await?;
                    let status = resp.status();
                    let retry_after = resp
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok());
                    let text = resp.text().await?;
                    Ok((status, retry_after, text))
                }
                .await;

            match outcome {
                Ok((status, retry_after, text)) => {
                    if status.is_success() {
                        return Ok((status, retry_after, text));
                    }
                    let err = parse_error(status, &text, retry_after);
                    if attempt < max_attempts && err.is_retryable() {
                        let wait = backoff(attempt, retry_after);
                        tracing::warn!(%status, code = err.code(), wait_ms = wait.as_millis() as u64, "retrying");
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    // Hand the body back; callers decide how to interpret non-2xx.
                    return Ok((status, retry_after, text));
                }
                Err(e) => {
                    let err = SarvaexError::Transport(e);
                    if attempt < max_attempts && err.is_retryable() {
                        let wait = backoff(attempt, None);
                        tracing::warn!(error = %err, wait_ms = wait.as_millis() as u64, "retrying after transport error");
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }
}

fn backoff(attempt: u32, retry_after: Option<u64>) -> Duration {
    if let Some(s) = retry_after {
        return Duration::from_secs(s.clamp(1, 60));
    }
    let base = 250u64 * 2u64.pow(attempt.min(6));
    // Deterministic jitter from the attempt number keeps this dependency-free.
    let jitter = (attempt as u64 * 37) % 100;
    Duration::from_millis(base + jitter)
}

fn decode<T: DeserializeOwned>(path: &str, text: &str) -> Result<T> {
    serde_json::from_str::<T>(text).map_err(|source| SarvaexError::Decode {
        endpoint: path.to_owned(),
        source,
        body: text.chars().take(500).collect(),
    })
}

fn parse_error(status: StatusCode, text: &str, retry_after: Option<u64>) -> SarvaexError {
    if let Ok(env) = serde_json::from_str::<ErrorEnvelope>(text) {
        let code = if env.error.code.is_empty() {
            format!("HTTP_{}", status.as_u16())
        } else {
            env.error.code
        };
        return SarvaexError::Api {
            status: status.as_u16(),
            code,
            message: env.error.message,
            retry_after,
        };
    }
    let snippet: String = text.chars().take(300).collect();
    SarvaexError::Api {
        status: status.as_u16(),
        code: format!("HTTP_{}", status.as_u16()),
        message: if snippet.is_empty() {
            status.canonical_reason().unwrap_or("error").to_owned()
        } else {
            snippet
        },
        retry_after,
    }
}

fn unwrap_field<T: DeserializeOwned>(v: Value, field: &str) -> Result<T> {
    let inner = v.get(field).cloned().unwrap_or(v);
    serde_json::from_value(inner).map_err(|source| SarvaexError::Decode {
        endpoint: field.to_owned(),
        source,
        body: String::new(),
    })
}

fn enc(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_derivation() {
        let b = normalize_base_url("https://api.sarvaex.com").unwrap();
        assert_eq!(
            derive_ws_url(&b).unwrap().as_str(),
            "wss://api.sarvaex.com/ws"
        );
        let l = normalize_base_url("http://localhost:18080").unwrap();
        assert_eq!(
            derive_ws_url(&l).unwrap().as_str(),
            "ws://localhost:18082/ws"
        );
        let bare = normalize_base_url("api.sarvaex.com").unwrap();
        assert_eq!(bare.as_str(), "https://api.sarvaex.com/");
    }

    #[test]
    fn credential_detection() {
        assert!(matches!(
            Credential::from_secret("svx_live_abc"),
            Credential::ApiKey(_)
        ));
        assert!(matches!(
            Credential::from_secret("eyJhbGciOi"),
            Credential::Bearer(_)
        ));
        assert!(matches!(Credential::from_secret("  "), Credential::None));
    }

    #[test]
    fn error_envelope_parsing() {
        let e = parse_error(
            StatusCode::CONFLICT,
            r#"{"error":{"code":"OPERATION_IN_PROGRESS","message":"wait"}}"#,
            None,
        );
        assert_eq!(e.code(), "OPERATION_IN_PROGRESS");
        assert!(e.is_retryable());
        let e = parse_error(StatusCode::BAD_GATEWAY, "upstream exploded", Some(3));
        assert_eq!(e.code(), "HTTP_502");
        assert!(e.is_retryable());
    }
}
