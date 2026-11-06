#![allow(clippy::type_complexity, clippy::result_large_err)]

//! A small in-memory imitation of `gw-rest` that reproduces the behaviours the
//! client stack has to handle: auth headers, scoped API keys, mandatory
//! idempotency keys with replay, rejected orders returned as 400 + OrderResponse,
//! `409 OPERATION_IN_PROGRESS`, `429` with `Retry-After`, and cursor pagination.
//!
//! It is used by the integration tests and can run standalone
//! (`sarvaex-mock-gateway --bind 127.0.0.1:18080`) so agent developers can
//! exercise the CLI/MCP without touching the real exchange.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const API_KEY_TRADER: &str = "svx_live_testkey0000000000000000000000000000000000000000000001";
pub const API_KEY_READONLY: &str = "svx_live_readonly000000000000000000000000000000000000000000002";
pub const USER_TRADER: &str = "alice_1";
pub const PASSWORD: &str = "a-long-password-12";

#[derive(Clone)]
pub struct MockGateway {
    state: Arc<Mutex<Inner>>,
}

struct Inner {
    contracts: Vec<Value>,
    books: HashMap<String, (Vec<(i64, i64)>, Vec<(i64, i64)>)>, // (bids, asks) as (price, qty)
    api_keys: HashMap<String, (String, Vec<String>)>,
    orders: HashMap<String, Value>,
    fills: Vec<Value>,
    balances: HashMap<String, i64>,
    positions: HashMap<(String, String), i64>,
    idempotency: HashMap<(String, String, String), (u16, Value, Value)>,
    in_progress_seen: HashMap<String, u32>,
    rate_limit_hits: u32,
    seq: i64,
    pub calls: Vec<String>,
}

fn contract(
    ticker: &str,
    kind: i64,
    question: &str,
    encoding: Value,
    category: &str,
    max_px: i64,
    state: i64,
) -> Value {
    json!({
        "ticker": ticker, "event_ticker": format!("XEV-{ticker}"), "series_ticker": "XLSX-STANDARD", "kind": kind,
        "question": question, "underlying": if kind == 2 { "cpi" } else { "" },
        "tick_size": 1, "min_price_ticks": 1, "max_price_ticks": max_px, "lower_bound_ticks": 1, "upper_bound_ticks": max_px,
        "multiplier_micro_usdc": 0, "divider": encoding["divider"], "multiplier_micro_per_display_unit": 0, "tick_value_micro": if kind == 2 { 10_000 } else { 0 },
        "max_order_size": 1000, "position_limit_per_user": 5000, "state": state,
        "listed_at": "2026-01-01T00:00:00Z", "open_at": "2026-01-01T00:00:00Z", "close_at": "2026-12-31T00:00:00Z", "expected_resolution_at": "2026-12-31T00:00:00Z",
        "settlement_source": "mock", "oracle_policy": "ADMIN",
        "settlement_rule": { "category": category, "encoding": encoding },
        "close_global_seq": 0, "payoff_rule": null, "payout_ticks": null
    })
}

impl Default for MockGateway {
    fn default() -> Self {
        Self::new()
    }
}

impl MockGateway {
    pub fn new() -> Self {
        let contracts = vec![
            contract(
                "SX-FEDDEC-26OCT-H25",
                1,
                "Will the FOMC raise rates by 25bp in Oct 2026?",
                json!({"divider":100,"decimals":2,"suffix":"¢"}),
                "Economics",
                99,
                3,
            ),
            contract(
                "SX-BTCD-26OCT30H17-T80000",
                1,
                "Will Bitcoin be above $80,000 at 17:00 UTC on 30 Oct 2026?",
                json!({"divider":100,"decimals":2,"suffix":"¢"}),
                "Crypto",
                99,
                3,
            ),
            contract(
                "SXF-USCPIYOY-26SEP",
                2,
                "US CPI YoY for Sep 2026 (%)",
                json!({"divider":100,"decimals":2,"suffix":"%"}),
                "Economics",
                2000,
                3,
            ),
            contract(
                "SX-EMPTYBOOK",
                1,
                "A market with no liquidity",
                json!({"divider":100,"decimals":2,"suffix":"¢"}),
                "Finance",
                99,
                3,
            ),
            contract(
                "SX-CLOSED",
                1,
                "A closed market",
                json!({"divider":100,"decimals":2,"suffix":"¢"}),
                "Finance",
                99,
                4,
            ),
        ];
        let mut books = HashMap::new();
        books.insert(
            "SX-FEDDEC-26OCT-H25".into(),
            (vec![(49, 120), (48, 80)], vec![(51, 96), (52, 60)]),
        );
        books.insert(
            "SX-BTCD-26OCT30H17-T80000".into(),
            (vec![(60, 10)], vec![(63, 25)]),
        );
        books.insert(
            "SXF-USCPIYOY-26SEP".into(),
            (vec![(429, 5)], vec![(431, 7)]),
        );
        books.insert("SX-EMPTYBOOK".into(), (vec![], vec![]));
        books.insert("SX-CLOSED".into(), (vec![], vec![]));
        let mut api_keys = HashMap::new();
        api_keys.insert(
            API_KEY_TRADER.to_owned(),
            (
                USER_TRADER.to_owned(),
                vec![
                    "markets:read".into(),
                    "account:read".into(),
                    "orders:read".into(),
                    "fills:read".into(),
                    "trading:write".into(),
                    "websocket:read".into(),
                ],
            ),
        );
        api_keys.insert(
            API_KEY_READONLY.to_owned(),
            (
                USER_TRADER.to_owned(),
                vec![
                    "markets:read".into(),
                    "account:read".into(),
                    "orders:read".into(),
                    "fills:read".into(),
                ],
            ),
        );
        let mut balances = HashMap::new();
        balances.insert(USER_TRADER.to_owned(), 10_000_000_000);
        Self {
            state: Arc::new(Mutex::new(Inner {
                contracts,
                books,
                api_keys,
                orders: HashMap::new(),
                fills: Vec::new(),
                balances,
                positions: HashMap::new(),
                idempotency: HashMap::new(),
                in_progress_seen: HashMap::new(),
                rate_limit_hits: 0,
                seq: 8200,
                calls: Vec::new(),
            })),
        }
    }

    /// Paths of every request received so far (for assertions).
    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn order(&self, id: &str) -> Option<Value> {
        self.state.lock().unwrap().orders.get(id).cloned()
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/ws", get(ws_upgrade))
            .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
            .route("/readyz", get(|| async { Json(json!({"status":"ready"})) }))
            .route("/v1/health/overview", get(health_overview))
            .route("/v1/auth/register", post(register))
            .route("/v1/auth/login", post(login))
            .route("/v1/account/profile", get(profile))
            .route("/v1/account/api-keys", get(list_keys).post(create_key))
            .route("/v1/account/api-keys/{key_id}", axum::routing::delete(revoke_key).post(revoke_key))
            .route("/v1/markets", get(list_markets))
            .route("/v1/markets/{ticker}", get(get_market))
            .route("/v1/markets/{ticker}/orderbook", get(orderbook))
            .route("/v1/markets/{ticker}/fills", get(market_fills))
            .route("/v1/markets/{ticker}/open-interest", get(open_interest))
            .route("/v1/series", get(|| async { Json(json!({"series":[{"series_ticker":"XLSX-STANDARD","title":"Standard","description":"mock"}],"next_cursor":""})) }))
            .route("/v1/events", get(|| async { Json(json!({"events":[],"next_cursor":""})) }))
            .route("/v1/orders", get(list_orders).post(submit_order))
            .route("/v1/orders/{order_id}", get(get_order))
            .route("/v1/orders/{order_id}/cancel", post(cancel_order))
            .route("/v1/account/balance", get(balance))
            .route("/v1/account/risk", get(risk))
            .route("/v1/account/margin/preview", post(preview_margin))
            .route("/v1/account/fills", get(account_fills))
            .route("/v1/account/history", get(|| async { Json(json!({"entries":[],"next_cursor":""})) }))
            .route("/v1/positions", get(positions))
            .route("/v1/demo/deposits/credit", post(demo_credit))
            .layer(axum::middleware::from_fn_with_state(self.clone(), record))
            .with_state(self.clone())
    }

    /// Bind to an ephemeral port and serve in the background. Returns the base URL.
    pub async fn spawn(&self) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = self.router();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        format!("http://{addr}")
    }
}

async fn record(
    State(gw): State<MockGateway>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    gw.state
        .lock()
        .unwrap()
        .calls
        .push(format!("{} {}", req.method(), req.uri().path()));
    next.run(req).await
}

fn err(status: StatusCode, code: &str, msg: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": msg}})),
    )
        .into_response()
}

/// Returns (user_id, scopes).
fn auth(gw: &MockGateway, headers: &HeaderMap) -> Result<(String, Vec<String>), Response> {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let token = v.strip_prefix("Bearer ").unwrap_or("");
        if let Some(user) = token.strip_prefix("mock.") {
            return Ok((user.to_owned(), vec!["*".into()]));
        }
        return Err(err(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "invalid bearer token",
        ));
    }
    if let Some(k) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        let st = gw.state.lock().unwrap();
        if let Some((user, scopes)) = st.api_keys.get(k.trim()) {
            return Ok((user.clone(), scopes.clone()));
        }
        return Err(err(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "invalid api key",
        ));
    }
    Err(err(
        StatusCode::UNAUTHORIZED,
        "UNAUTHENTICATED",
        "authentication required",
    ))
}

fn require_scope(scopes: &[String], scope: &str) -> Result<(), Response> {
    if scopes.iter().any(|s| s == "*" || s == scope) {
        Ok(())
    } else {
        Err(err(
            StatusCode::FORBIDDEN,
            "PERMISSION_DENIED",
            "credential does not have the required scope",
        ))
    }
}

async fn health_overview() -> Json<Value> {
    Json(
        json!({"summary": {"running": 3, "total": 3, "not_running": 0}, "items": [{"name": "mock", "kind": "http", "status": "running", "latency_ms": 1, "detail": "ok"}]}),
    )
}

async fn register(State(gw): State<MockGateway>, Json(body): Json<Value>) -> Response {
    let user = body["user_id"].as_str().unwrap_or("").to_owned();
    if user.len() < 3 {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "user_id too short",
        );
    }
    gw.state
        .lock()
        .unwrap()
        .balances
        .entry(user.clone())
        .or_insert(0);
    Json(json!({"token": format!("mock.{user}"), "token_type": "Bearer", "name": body["name"], "user_id": user, "email": body["email"]})).into_response()
}

async fn login(Json(body): Json<Value>) -> Response {
    let pw = body["password"].as_str().unwrap_or("");
    if pw != PASSWORD {
        return err(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "invalid credentials",
        );
    }
    let user = body["user_id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| USER_TRADER.to_owned());
    Json(json!({"token": format!("mock.{user}"), "token_type": "Bearer", "name": "Alice Trader", "user_id": user, "email": "alice@example.com"})).into_response()
}

async fn profile(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    Json(json!({"user_id": user, "name": "Alice Trader", "email": "alice@example.com", "status": "ACTIVE", "created_at": "2026-01-01T00:00:00Z", "has_password": true})).into_response()
}

async fn list_keys(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = gw.state.lock().unwrap();
    let keys: Vec<Value> = st
        .api_keys
        .iter()
        .filter(|(_, (u, _))| *u == user)
        .map(|(k, (_, scopes))| json!({"key_id": format!("kid-{}", &k[9..17]), "key_prefix": &k[..17], "name": "key", "scopes": scopes, "created_at": "2026-01-01T00:00:00Z", "expires_at": null, "last_used_at": null, "revoked_at": null}))
        .collect();
    Json(json!({"api_keys": keys})).into_response()
}

async fn create_key(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let scopes: Vec<String> = body["scopes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let raw = format!("svx_live_{}{}", uuid_simple(), uuid_simple());
    let prefix: String = raw.chars().take(17).collect();
    gw.state
        .lock()
        .unwrap()
        .api_keys
        .insert(raw.clone(), (user, scopes.clone()));
    Json(json!({"key_id": format!("kid-{}", &prefix[9..]), "name": body["name"], "key": raw, "key_prefix": prefix, "scopes": scopes, "created_at": "2026-01-01T00:00:00Z", "expires_at": body["expires_at"]})).into_response()
}

async fn revoke_key(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
) -> Response {
    if let Err(r) = auth(&gw, &headers) {
        return r;
    }
    let suffix = key_id.trim_start_matches("kid-").to_owned();
    gw.state
        .lock()
        .unwrap()
        .api_keys
        .retain(|k, _| !k[9..17].eq(&suffix));
    Json(json!({"ok": true, "key_id": key_id})).into_response()
}

fn uuid_simple() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    let n = N
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!("{n:016x}{:016x}", n.rotate_left(17))
}

async fn list_markets(
    State(gw): State<MockGateway>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let st = gw.state.lock().unwrap();
    let state_filter = q.get("state").map(|s| match s.as_str() {
        "OPEN" => 3,
        "CLOSED" => 4,
        "SETTLED" => 6,
        _ => 0,
    });
    let kind_filter = q.get("kind").map(|k| if k == "BINARY" { 1 } else { 2 });
    let items: Vec<Value> = st
        .contracts
        .iter()
        .filter(|c| state_filter.is_none_or(|s| c["state"] == s))
        .filter(|c| kind_filter.is_none_or(|k| c["kind"] == k))
        .filter(|c| {
            q.get("category")
                .is_none_or(|cat| c["settlement_rule"]["category"] == cat.as_str())
        })
        .cloned()
        .collect();
    let limit: usize = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
    let offset: usize = q.get("cursor").and_then(|c| c.parse().ok()).unwrap_or(0);
    let page: Vec<Value> = items.iter().skip(offset).take(limit).cloned().collect();
    let next = if offset + limit < items.len() {
        (offset + limit).to_string()
    } else {
        String::new()
    };
    Json(json!({"contracts": page, "next_cursor": next})).into_response()
}

async fn get_market(State(gw): State<MockGateway>, Path(ticker): Path<String>) -> Response {
    if ticker == "SX-RATELIMIT" {
        let mut st = gw.state.lock().unwrap();
        st.rate_limit_hits += 1;
        if st.rate_limit_hits % 2 == 1 {
            return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")], Json(json!({"error": {"code": "RATE_LIMITED", "message": "too many requests, slow down"}}))).into_response();
        }
        return Json(st.contracts[0].clone()).into_response();
    }
    let st = gw.state.lock().unwrap();
    match st.contracts.iter().find(|c| c["ticker"] == ticker) {
        Some(c) => Json(c.clone()).into_response(),
        None => err(StatusCode::NOT_FOUND, "NOT_FOUND", "contract not found"),
    }
}

async fn orderbook(
    State(gw): State<MockGateway>,
    Path(ticker): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let st = gw.state.lock().unwrap();
    let Some((bids, asks)) = st.books.get(&ticker) else {
        return err(StatusCode::NOT_FOUND, "NOT_FOUND", "contract not found");
    };
    let depth: usize = q.get("depth").and_then(|d| d.parse().ok()).unwrap_or(20);
    let lv = |(p, q): &(i64, i64)| json!({"price_ticks": p, "total_qty": q, "order_count": 1});
    Json(json!({"ticker": ticker, "seq": st.seq, "ts": "2026-09-25T09:10:00Z", "bids": bids.iter().take(depth).map(lv).collect::<Vec<_>>(), "asks": asks.iter().take(depth).map(lv).collect::<Vec<_>>()})).into_response()
}

async fn market_fills(State(gw): State<MockGateway>, Path(ticker): Path<String>) -> Response {
    let st = gw.state.lock().unwrap();
    let fills: Vec<Value> = st
        .fills
        .iter()
        .filter(|f| f["ticker"] == ticker)
        .cloned()
        .collect();
    Json(json!({"fills": fills, "next_cursor": ""})).into_response()
}

async fn open_interest(State(gw): State<MockGateway>, Path(ticker): Path<String>) -> Response {
    let st = gw.state.lock().unwrap();
    Json(json!({"ticker": ticker, "total_open_long": 120, "total_open_short": 120, "as_of_global_seq": st.seq, "as_of": "2026-09-25T09:10:00Z"})).into_response()
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, Response> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                "Idempotency-Key header is required",
            )
        })
}

async fn submit_order(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (user, scopes) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_scope(&scopes, "trading:write") {
        return r;
    }
    let key = match idempotency_key(&headers) {
        Ok(k) => k,
        Err(r) => return r,
    };
    let mut st = gw.state.lock().unwrap();
    let idem = (user.clone(), "POST /v1/orders".to_owned(), key.clone());
    if let Some((status, stored_body, resp)) = st.idempotency.get(&idem) {
        if *stored_body != body {
            return err(
                StatusCode::CONFLICT,
                "IDEMPOTENCY_KEY_REUSED",
                "idempotency key reused with a different body",
            );
        }
        return (StatusCode::from_u16(*status).unwrap(), Json(resp.clone())).into_response();
    }
    if key.starts_with("inprogress-") {
        let n = st.in_progress_seen.entry(key.clone()).or_insert(0);
        *n += 1;
        if *n == 1 {
            return err(
                StatusCode::CONFLICT,
                "OPERATION_IN_PROGRESS",
                "retry with the same key",
            );
        }
    }

    let ticker = body["ticker"].as_str().unwrap_or("").to_owned();
    let Some(contract) = st.contracts.iter().find(|c| c["ticker"] == ticker).cloned() else {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "unknown ticker",
        );
    };
    if contract["state"] != 3 {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "contract is not open",
        );
    }
    let side = body["side"].as_str().unwrap_or("");
    if side != side.to_ascii_uppercase() {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "side must use an uppercase enum value",
        );
    }
    let binary = contract["kind"] == 1;
    if (binary && !matches!(side, "YES" | "NO")) || (!binary && !matches!(side, "LONG" | "SHORT")) {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "side does not match the contract kind",
        );
    }
    let count = body["count"].as_i64().unwrap_or(0);
    if count <= 0 {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "count must be positive",
        );
    }
    let order_type = body["order_type"]
        .as_str()
        .or(body["type"].as_str())
        .unwrap_or("LIMIT");
    let market = order_type == "MARKET";
    let action = body["action"].as_str().unwrap_or("BUY");
    let mut price = body["price_ticks"].as_i64().unwrap_or(0);
    // In the engine's normalised YES/LONG book, buying YES or selling NO lifts asks.
    let takes_asks = (action == "BUY") == matches!(side, "YES" | "LONG");
    let (bids, asks) = st.books.get(&ticker).cloned().unwrap_or_default();
    let opposite = if takes_asks {
        asks.first()
    } else {
        bids.first()
    };
    if market {
        match opposite {
            Some((p, _)) => price = if binary && side == "NO" { 100 - *p } else { *p },
            None => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "MARKET_NOT_AVAILABLE",
                    "no live opposite-side quote",
                );
            }
        }
    } else if price <= 0 {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "price_ticks must be positive",
        );
    }
    // Binary NO orders are quoted in NO cents; normalise to the YES book for matching.
    let book_price = if binary && side == "NO" {
        100 - price
    } else {
        price
    };
    let max_order = contract["max_order_size"].as_i64().unwrap_or(0);
    st.seq += 1;
    let order_id = format!("ord-{}", st.seq);
    let tif = body["tif"]
        .as_str()
        .filter(|t| !t.is_empty())
        .unwrap_or(if market { "IOC" } else { "GTC" });

    let (status, reject_code, reject_reason, filled, fills) = if count > max_order {
        (
            "REJECTED",
            "MAX_ORDER_SIZE_EXCEEDED",
            "count exceeds max order size",
            0,
            vec![],
        )
    } else if count == 777 {
        // Simulates a risk-service rejection that passes client-side validation.
        (
            "REJECTED",
            "POSITION_LIMIT_EXCEEDED",
            "position limit exceeded",
            0,
            vec![],
        )
    } else {
        let crosses = match opposite {
            Some((p, _)) => {
                if takes_asks {
                    book_price >= *p
                } else {
                    book_price <= *p
                }
            }
            None => false,
        };
        if crosses {
            let (p, avail) = opposite.unwrap();
            let fill_qty = count.min(*avail);
            let fill_px = if binary && side == "NO" { 100 - p } else { *p };
            let fill = json!({"fill_id": format!("fill-{}", st.seq), "order_id": order_id, "ticker": ticker, "price_ticks": fill_px, "count": fill_qty, "aggressor_side": side, "fee_micro_usdc": 0, "ts": "2026-09-25T09:10:00Z", "seq": st.seq});
            let status = if fill_qty == count {
                "FILLED"
            } else if tif == "IOC" || tif == "FOK" {
                "CANCELLED"
            } else {
                "PARTIAL"
            };
            if tif == "FOK" && fill_qty < count {
                ("CANCELLED", "", "", 0, vec![])
            } else {
                (status, "", "", fill_qty, vec![fill])
            }
        } else if tif == "IOC" || tif == "FOK" {
            ("CANCELLED", "", "", 0, vec![])
        } else {
            ("OPEN", "", "", 0, vec![])
        }
    };
    let remaining = if status == "OPEN" || status == "PARTIAL" {
        count - filled
    } else {
        0
    };
    let cancelled = if status == "CANCELLED" {
        count - filled
    } else {
        0
    };
    let avg = fills
        .first()
        .and_then(|f| f["price_ticks"].as_i64())
        .unwrap_or(0);
    let order = json!({
        "order_id": order_id, "client_order_id": body["client_order_id"], "user_id": user, "ticker": ticker, "side": side, "action": action,
        "price_ticks": price, "count": count, "filled_count": filled, "remaining_count": remaining, "cancelled_count": cancelled, "expired_count": 0,
        "tif": tif, "post_only": body["post_only"].as_bool().unwrap_or(false), "reduce_only": body["reduce_only"].as_bool().unwrap_or(false), "stp": "UNSPECIFIED",
        "status": status, "created_at": "2026-09-25T09:10:00Z", "updated_at": "2026-09-25T09:10:00Z", "expires_at": null, "hold_id": format!("hold-{}", st.seq), "avg_fill_price_ticks": avg,
    });
    if filled > 0 {
        let signed = if matches!(side, "YES" | "LONG") == (action == "BUY") {
            filled
        } else {
            -filled
        };
        *st.positions
            .entry((user.clone(), ticker.clone()))
            .or_insert(0) += signed;
        for f in &fills {
            let mut pf = f.clone();
            pf["maker_side"] = json!("YES");
            pf["taker_side"] = json!(side);
            pf["global_seq"] = json!(st.seq);
            st.fills.push(pf);
        }
    }
    st.orders.insert(order_id.clone(), order.clone());
    let resp = json!({"order": order, "fills": fills, "reject_code": reject_code, "reject_reason": reject_reason});
    let http = if reject_code.is_empty() { 200 } else { 400 };
    st.idempotency.insert(idem, (http, body, resp.clone()));
    (StatusCode::from_u16(http).unwrap(), Json(resp)).into_response()
}

async fn list_orders(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = gw.state.lock().unwrap();
    let mut orders: Vec<Value> = st
        .orders
        .values()
        .filter(|o| o["user_id"] == user)
        .filter(|o| q.get("ticker").is_none_or(|t| o["ticker"] == t.as_str()))
        .filter(|o| q.get("status").is_none_or(|s| o["status"] == s.as_str()))
        .cloned()
        .collect();
    orders.sort_by(|a, b| a["order_id"].as_str().cmp(&b["order_id"].as_str()));
    Json(json!({"orders": orders, "next_cursor": ""})).into_response()
}

async fn get_order(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Path(order_id): Path<String>,
) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = gw.state.lock().unwrap();
    match st.orders.get(&order_id) {
        Some(o) if o["user_id"] == user => Json(o.clone()).into_response(),
        _ => err(StatusCode::NOT_FOUND, "NOT_FOUND", "order not found"),
    }
}

async fn cancel_order(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Path(order_id): Path<String>,
) -> Response {
    let (user, scopes) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_scope(&scopes, "trading:write") {
        return r;
    }
    if let Err(r) = idempotency_key(&headers) {
        return r;
    }
    let mut st = gw.state.lock().unwrap();
    let Some(order) = st.orders.get_mut(&order_id) else {
        return err(StatusCode::NOT_FOUND, "NOT_FOUND", "order not found");
    };
    if order["user_id"] != user {
        return err(StatusCode::NOT_FOUND, "NOT_FOUND", "order not found");
    }
    let status = order["status"].as_str().unwrap_or("").to_owned();
    if !matches!(status.as_str(), "OPEN" | "PARTIAL") {
        let resp = json!({"order": order.clone(), "fills": [], "reject_code": "ORDER_TERMINAL", "reject_reason": "order is already terminal"});
        return (StatusCode::BAD_REQUEST, Json(resp)).into_response();
    }
    let remaining = order["remaining_count"].as_i64().unwrap_or(0);
    order["status"] = json!("CANCELLED");
    order["cancelled_count"] = json!(remaining);
    order["remaining_count"] = json!(0);
    Json(json!({"order": order.clone(), "fills": [], "reject_code": "", "reject_reason": ""}))
        .into_response()
}

async fn balance(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = gw.state.lock().unwrap();
    let cash = *st.balances.get(&user).unwrap_or(&0);
    Json(json!({"user_id": user, "cash_micro_usdc": cash, "held_micro_usdc": 5_100_000, "total_micro_usdc": cash + 5_100_000})).into_response()
}

async fn risk(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = gw.state.lock().unwrap();
    let cash = *st.balances.get(&user).unwrap_or(&0);
    Json(json!({
        "user_id": user, "cash_micro_usdc": cash, "held_micro_usdc": 0, "total_micro_usdc": cash, "available_collateral_micro_usdc": cash,
        "equity_micro_usdc": cash, "position_cost_micro_usdc": 0, "realized_pnl_micro_usdc": 0, "unrealized_pnl_micro_usdc": 0,
        "open_positions": st.positions.len(), "open_orders": st.orders.values().filter(|o| o["status"] == "OPEN").count(), "as_of_global_seq": st.seq,
        "limits": {"user_id": user, "kyc_tier": 1, "max_order_size_micro_usdc": 100_000_000, "daily_loss_limit_micro_usdc": 10_000_000, "per_contract_position_limit": {}},
        "calculation_basis": {}
    }))
    .into_response()
}

async fn preview_margin(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = auth(&gw, &headers) {
        return r;
    }
    let count = body["count"].as_i64().unwrap_or(0);
    Json(json!({"event_ticker": format!("XEV-{}", body["ticker"].as_str().unwrap_or("")), "eligible": true, "ineligible_reason": "", "margin_before_micro_usdc": 0, "margin_after_micro_usdc": count * 1_000_000, "delta_micro_usdc": count * 1_000_000, "standalone_delta_micro_usdc": count * 1_000_000})).into_response()
}

async fn account_fills(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = gw.state.lock().unwrap();
    let fills: Vec<Value> = st
        .fills
        .iter()
        .filter(|f| st.orders.get(f["order_id"].as_str().unwrap_or("")).is_some_and(|o| o["user_id"] == user))
        .map(|f| json!({"fill_id": f["fill_id"], "ticker": f["ticker"], "global_seq": f["global_seq"], "contract_seq": f["seq"], "order_id": f["order_id"], "role": "TAKER", "side": f["taker_side"], "action": "BUY", "price_ticks": f["price_ticks"], "count": f["count"], "aggressor_side": f["taker_side"], "fee_micro_usdc": 0, "ts": f["ts"]}))
        .collect();
    Json(json!({"fills": fills, "next_cursor": ""})).into_response()
}

async fn positions(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = gw.state.lock().unwrap();
    let items: Vec<Value> = st
        .positions
        .iter()
        .filter(|((u, _), q)| *u == user && **q != 0)
        .map(|((_, t), q)| json!({"user_id": user, "ticker": t, "net_qty": q, "avg_cost_micro_usdc": 510_000, "realized_pnl_micro_usdc": 0, "unrealized_pnl_micro_usdc": 0, "updated_at": "2026-09-25T09:10:00Z", "last_global_seq": st.seq}))
        .collect();
    Json(json!({"positions": items, "next_cursor": ""})).into_response()
}

async fn demo_credit(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let (user, scopes) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_scope(&scopes, "trading:write") {
        return r;
    }
    if let Err(r) = idempotency_key(&headers) {
        return r;
    }
    let micro = body["amount_micro_usdc"]
        .as_i64()
        .or_else(|| body["amount_usdc"].as_i64().map(|u| u * 1_000_000))
        .unwrap_or(0);
    if micro <= 0 {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "amount must be positive",
        );
    }
    let mut st = gw.state.lock().unwrap();
    let bal = st.balances.entry(user.clone()).or_insert(0);
    *bal += micro;
    let cash = *bal;
    Json(json!({"ok": true, "balance": {"user_id": user, "cash_micro_usdc": cash, "held_micro_usdc": 0, "total_micro_usdc": cash}})).into_response()
}

// ---------------------------------------------------------------------------
// WebSocket (mirrors gw-ws message shapes)
// ---------------------------------------------------------------------------

async fn ws_upgrade(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let authed = auth(&gw, &headers)
        .ok()
        .filter(|(_, scopes)| scopes.iter().any(|s| s == "*" || s == "websocket:read"))
        .map(|(u, _)| u);
    ws.on_upgrade(move |socket| ws_session(gw, socket, authed))
}

async fn ws_session(gw: MockGateway, socket: WebSocket, authed: Option<String>) {
    let (mut tx, mut rx) = socket.split();
    let _ = tx
        .send(Message::Text(
            json!({"type": "connected", "service": "gw-ws"})
                .to_string()
                .into(),
        ))
        .await;
    while let Some(Ok(msg)) = rx.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let channel = req["channel"].as_str().unwrap_or("");
        let ticker = req["ticker"].as_str().unwrap_or("").to_owned();
        if req["op"] != "subscribe" {
            continue;
        }
        if channel == "private" && authed.is_none() {
            let _ = tx.send(Message::Text(json!({"type": "error", "code": "UNAUTHENTICATED", "message": "private channel requires websocket:read"}).to_string().into())).await;
            continue;
        }
        let _ = tx
            .send(Message::Text(
                json!({"type": "subscribed", "channel": channel, "ticker": ticker})
                    .to_string()
                    .into(),
            ))
            .await;
        let (bids, asks, seq) = {
            let st = gw.state.lock().unwrap();
            let (b, a) = st.books.get(&ticker).cloned().unwrap_or_default();
            (b, a, st.seq)
        };
        let lv = |(p, q): &(i64, i64)| json!({"price_ticks": p, "total_qty": q, "order_count": 1});
        if channel == "market" {
            let frames = vec![
                json!({"type": "market_book_snapshot", "ticker": ticker, "seq": seq, "book_seq": seq, "bids": bids.iter().map(lv).collect::<Vec<_>>(), "asks": asks.iter().map(lv).collect::<Vec<_>>()}),
                json!({"type": "market_book_delta", "event_id": "evt-1", "ticker": ticker, "global_seq": seq + 1, "contract_seq": 1, "book_seq": seq + 1, "book_side": "BID", "side": "YES", "price_ticks": 49, "qty_delta": -20, "new_total_qty": 100}),
                json!({"type": "market_trade", "event_id": "evt-2", "ticker": ticker, "global_seq": seq + 2, "contract_seq": 2, "price_ticks": 51, "count": 3, "aggressor_side": "YES"}),
                // Deliberate gap (book_seq jumps by 2) so clients exercise resync.
                json!({"type": "market_book_delta", "event_id": "evt-3", "ticker": ticker, "global_seq": seq + 4, "contract_seq": 4, "book_seq": seq + 4, "book_side": "ASK", "side": "YES", "price_ticks": 51, "qty_delta": -3, "new_total_qty": 93}),
            ];
            for f in frames {
                let _ = tx.send(Message::Text(f.to_string().into())).await;
            }
        } else if channel == "private" {
            let _ = tx
                .send(Message::Text(json!({"type": "private_fill", "event_id": "evt-5", "ticker": ticker, "global_seq": seq + 5, "contract_seq": 5, "order_id": "ord-x", "role": "TAKER", "price_ticks": 51, "count": 1, "aggressor_side": "YES"}).to_string().into()))
                .await;
        }
    }
}
