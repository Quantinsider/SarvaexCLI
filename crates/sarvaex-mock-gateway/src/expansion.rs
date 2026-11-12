//! Mock of the gateway's expanded API (amend, batch and bulk cancel, close position,
//! dead man's switch, candles/trades/stats, fees, activity, settlements, exchange
//! status, key limits). Behaviour is simplified but follows the documented shapes
//! and error codes so client code can be exercised offline.

use super::{MockGateway, auth, err, idempotency_key, require_scope};
use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use std::collections::HashMap;

pub fn routes() -> Router<MockGateway> {
    Router::new()
        .route("/v1/time", get(time))
        .route("/v1/exchange/status", get(exchange_status))
        .route("/v1/exchange/schedule", get(exchange_schedule))
        .route("/v1/fees", get(fees))
        .route("/v1/markets/stats", get(markets_stats))
        .route("/v1/markets/{ticker}/stats", get(market_stats))
        .route("/v1/markets/{ticker}/trades", get(trades))
        .route("/v1/markets/{ticker}/candles", get(candles))
        .route("/v1/orderbooks", get(orderbooks))
        .route("/v1/orders/{order_id}/amend", post(amend))
        .route("/v1/orders/batch", post(batch))
        .route("/v1/orders/cancel-batch", post(cancel_batch))
        .route("/v1/orders/cancel-all", post(cancel_all))
        .route("/v1/positions/{ticker}/close", post(close_position))
        .route(
            "/v1/account/cancel-after",
            get(cancel_after_get).post(cancel_after_set),
        )
        .route("/v1/account/fees", get(account_fees))
        .route("/v1/account/incentives", get(account_incentives))
        .route("/v1/account/settlements", get(account_settlements))
        .route("/v1/account/activity", get(account_activity))
        .route(
            "/v1/account/api-keys/{key_id}/limits",
            axum::routing::patch(patch_limits),
        )
}

fn status_body() -> Value {
    json!({
        "mode": "OPEN", "configured_mode": "OPEN", "exchange_active": true,
        "new_orders_allowed": true, "cancels_allowed": true, "in_maintenance": false,
        "maintenance_starts_at": null, "maintenance_ends_at": null, "message": "",
        "protocol_version": "v1"
    })
}

async fn time() -> Json<Value> {
    Json(json!({"epoch_ms": 1_794_509_989_879_i64, "server_time": "2026-11-12T18:59:49.879Z"}))
}

async fn exchange_status() -> Json<Value> {
    Json(status_body())
}

async fn exchange_schedule() -> Json<Value> {
    Json(json!({
        "contracts_closing_next_7d": [{"ticker": "SX-BTCD-26OCT30H17-T80000", "close_at": "2026-10-30T17:00:00Z", "state": "OPEN"}],
        "status": status_body()
    }))
}

async fn fees() -> Json<Value> {
    Json(json!({
        "taker_fee_bps": 0, "maker_fee_bps": 0, "maker_incentive_bps": 0,
        "fee_basis": "the price of the side you trade, in micro-USDC",
        "rounding": "fees round up to the micro",
        "example": {"notional_micro_usdc": 5_100_000, "taker_fee_micro_usdc": 0, "maker_incentive_micro_usdc": 0}
    }))
}

fn stats_for(st: &super::Inner, ticker: &str) -> Value {
    let last = st
        .fills
        .iter()
        .rev()
        .find(|f| f["ticker"] == ticker)
        .and_then(|f| f["price_ticks"].as_i64());
    json!({
        "ticker": ticker, "last_price_ticks": last, "last_trade_ts": null,
        "price_change_24h_ticks": null, "high_24h_ticks": last, "low_24h_ticks": last,
        "volume_24h": st.fills.iter().filter(|f| f["ticker"] == ticker).count(),
        "trades_24h": st.fills.iter().filter(|f| f["ticker"] == ticker).count(),
        "volume_total": 0
    })
}

async fn market_stats(State(gw): State<MockGateway>, Path(ticker): Path<String>) -> Response {
    let st = gw.state.lock().unwrap();
    if !st.contracts.iter().any(|c| c["ticker"] == ticker) {
        return err(StatusCode::NOT_FOUND, "NOT_FOUND", "market not found");
    }
    Json(stats_for(&st, &ticker)).into_response()
}

async fn markets_stats(
    State(gw): State<MockGateway>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let st = gw.state.lock().unwrap();
    let tickers: Vec<&str> = q
        .get("tickers")
        .map(|t| t.split(',').filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    if tickers.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "tickers is required, e.g. tickers=SX-A,SX-B",
        );
    }
    let stats: Vec<Value> = tickers.iter().map(|t| stats_for(&st, t)).collect();
    Json(json!({"count": stats.len(), "stats": stats})).into_response()
}

async fn trades(State(gw): State<MockGateway>, Path(ticker): Path<String>) -> Response {
    let st = gw.state.lock().unwrap();
    let rows: Vec<Value> = st
        .fills
        .iter()
        .rev()
        .filter(|f| f["ticker"] == ticker)
        .map(|f| json!({"trade_id": f["fill_id"], "price_ticks": f["price_ticks"], "count": f["count"], "aggressor_side": f["aggressor_side"], "ts": f["ts"], "global_seq": f["global_seq"], "contract_seq": f["global_seq"]}))
        .collect();
    Json(json!({"ticker": ticker, "trades": rows})).into_response()
}

const INTERVALS: [&str; 6] = ["1m", "5m", "15m", "1h", "4h", "1d"];

async fn candles(
    State(gw): State<MockGateway>,
    Path(ticker): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let interval = q.get("interval").map(String::as_str).unwrap_or("1h");
    if !INTERVALS.contains(&interval) {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "interval must be one of 1m, 5m, 15m, 1h, 4h, 1d",
        );
    }
    let st = gw.state.lock().unwrap();
    let prices: Vec<i64> = st
        .fills
        .iter()
        .filter(|f| f["ticker"] == ticker)
        .filter_map(|f| f["price_ticks"].as_i64())
        .collect();
    let rows: Vec<Value> = if prices.is_empty() {
        vec![]
    } else {
        vec![json!({
            "ts": "2026-11-12T18:00:00Z", "open": prices[0], "high": prices.iter().max(), "low": prices.iter().min(),
            "close": prices.last(), "volume": prices.len(), "trades": prices.len(), "vwap": prices[0]
        })]
    };
    Json(json!({"ticker": ticker, "interval": interval, "interval_seconds": 3600, "price_unit": "ticks", "candles": rows}))
        .into_response()
}

async fn orderbooks(
    State(gw): State<MockGateway>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let st = gw.state.lock().unwrap();
    let tickers: Vec<&str> = q
        .get("tickers")
        .map(|t| t.split(',').filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    if tickers.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "tickers is required, e.g. tickers=SX-A,SX-B",
        );
    }
    let mut books = Vec::new();
    let mut errors = Vec::new();
    for t in tickers {
        if !st.contracts.iter().any(|c| c["ticker"] == t) {
            errors.push(json!({"ticker": t, "code": "NOT_FOUND"}));
            continue;
        }
        let (bids, asks) = st.books.get(t).cloned().unwrap_or_default();
        let lv = |v: &Vec<(i64, i64)>| -> Vec<Value> {
            v.iter()
                .map(|(p, n)| json!({"price_ticks": p, "total_qty": n, "order_count": 1}))
                .collect()
        };
        let best_bid = bids.iter().map(|b| b.0).max();
        let best_ask = asks.iter().map(|a| a.0).min();
        books.push(json!({
            "ticker": t, "seq": st.seq, "book_seq": st.seq, "ts": "2026-11-12T19:00:00Z",
            "bids": lv(&bids), "asks": lv(&asks),
            "best": {"yes_bid": best_bid, "yes_ask": best_ask, "no_bid": best_ask.map(|a| 100 - a), "no_ask": best_bid.map(|b| 100 - b)}
        }));
    }
    Json(json!({"count": books.len(), "orderbooks": books, "errors": errors})).into_response()
}

// ---------------------------------------------------------------------------
// trading
// ---------------------------------------------------------------------------

fn trader(gw: &MockGateway, headers: &HeaderMap) -> Result<String, Response> {
    let (user, scopes) = auth(gw, headers)?;
    require_scope(&scopes, "trading:write")?;
    idempotency_key(headers)?;
    Ok(user)
}

async fn amend(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Path(order_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let user = match trader(&gw, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let mut st = gw.state.lock().unwrap();
    let Some(order) = st.orders.get_mut(&order_id) else {
        return err(StatusCode::NOT_FOUND, "NOT_FOUND", "order not found");
    };
    if order["user_id"] != user {
        return err(StatusCode::NOT_FOUND, "NOT_FOUND", "order not found");
    }
    if !matches!(order["status"].as_str(), Some("OPEN" | "PARTIAL")) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"order": order.clone(), "reject_code": "ORDER_TERMINAL", "reject_reason": "order is already terminal"})),
        )
            .into_response();
    }
    if body["price_ticks"].is_null() && body["count"].is_null() {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "price_ticks and/or count is required",
        );
    }
    // A count change reconciles the hold in the real router, so it is replaced; a
    // price-only change is amended in place.
    let via = if body["count"].is_null() {
        "IN_PLACE"
    } else {
        "CANCEL_REPLACE"
    };
    if let Some(p) = body["price_ticks"].as_i64() {
        order["price_ticks"] = json!(p);
    }
    if let Some(c) = body["count"].as_i64() {
        order["count"] = json!(c);
        order["remaining_count"] = json!(c - order["filled_count"].as_i64().unwrap_or(0));
    }
    let mut resp =
        json!({"amended_via": via, "order": order.clone(), "reject_code": "", "reject_reason": ""});
    if via == "CANCEL_REPLACE" {
        resp["cancelled_order"] = order.clone();
    }
    Json(resp).into_response()
}

async fn batch(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let user = match trader(&gw, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let orders = body["orders"].as_array().cloned().unwrap_or_default();
    if orders.is_empty() || orders.len() > 20 {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "orders must hold 1 to 20 entries",
        );
    }
    let mut st = gw.state.lock().unwrap();
    let mut results = Vec::new();
    let (mut accepted, mut failed) = (0, 0);
    for (index, o) in orders.iter().enumerate() {
        let bad = o["price_ticks"].as_i64().unwrap_or(0) <= 0 && o["order_type"] != "MARKET";
        if bad {
            failed += 1;
            results.push(json!({"index": index, "status": 400, "body": {"error": {"code": "INVALID_ARGUMENT", "message": "price_ticks must be positive"}}}));
            continue;
        }
        st.seq += 1;
        let id = format!("ord_batch_{}", st.seq);
        let count = o["count"].as_i64().unwrap_or(1);
        let order = json!({
            "order_id": id, "user_id": user, "client_order_id": o["client_order_id"], "ticker": o["ticker"],
            "side": o["side"], "action": o["action"], "price_ticks": o["price_ticks"], "count": count,
            "filled_count": 0, "remaining_count": count, "cancelled_count": 0, "expired_count": 0,
            "status": "OPEN", "created_at": "2026-11-12T19:00:00Z"
        });
        st.orders.insert(id, order.clone());
        accepted += 1;
        results.push(json!({"index": index, "status": 200, "body": {"order": order, "fills": [], "reject_code": "", "reject_reason": ""}}));
    }
    Json(json!({"results": results, "accepted": accepted, "failed": failed})).into_response()
}

fn cancel_ids(st: &mut super::Inner, user: &str, ids: &[String]) -> Value {
    let (mut cancelled, mut failed) = (Vec::new(), Vec::new());
    for id in ids {
        match st.orders.get_mut(id) {
            Some(o) if o["user_id"] == user && matches!(o["status"].as_str(), Some("OPEN" | "PARTIAL" | "TRIGGER_PENDING")) => {
                o["status"] = json!(if o["status"] == "TRIGGER_PENDING" { "TRIGGER_CANCELLED" } else { "CANCELLED" });
                o["remaining_count"] = json!(0);
                cancelled.push(json!(id));
            }
            _ => failed.push(json!({"order_id": id, "reject_code": "ORDER_NOT_CANCELABLE", "reject_reason": "unknown or already terminal"})),
        }
    }
    json!({"cancelled": cancelled, "failed": failed})
}

async fn cancel_batch(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let user = match trader(&gw, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let ids: Vec<String> = body["order_ids"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() || ids.len() > 50 {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "order_ids must hold 1 to 50 entries",
        );
    }
    let mut st = gw.state.lock().unwrap();
    Json(cancel_ids(&mut st, &user, &ids)).into_response()
}

async fn cancel_all(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let user = match trader(&gw, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let mut st = gw.state.lock().unwrap();
    let ticker = body["ticker"].as_str().map(str::to_owned);
    let ids: Vec<String> = st
        .orders
        .iter()
        .filter(|(_, o)| {
            o["user_id"] == user
                && matches!(
                    o["status"].as_str(),
                    Some("OPEN" | "PARTIAL" | "TRIGGER_PENDING")
                )
                && ticker.as_deref().is_none_or(|t| o["ticker"] == t)
        })
        .map(|(id, _)| id.clone())
        .collect();
    let mut resp = cancel_ids(&mut st, &user, &ids);
    resp["ticker"] = json!(ticker);
    Json(resp).into_response()
}

async fn close_position(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Path(ticker): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let user = match trader(&gw, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let mut st = gw.state.lock().unwrap();
    let net = st
        .positions
        .get(&(user.clone(), ticker.clone()))
        .copied()
        .unwrap_or(0);
    if net == 0 {
        return err(
            StatusCode::BAD_REQUEST,
            "NO_POSITION",
            "there is no open position to close on this contract",
        );
    }
    let count = body["count"].as_i64().unwrap_or(net.abs()).min(net.abs());
    st.positions
        .insert((user.clone(), ticker.clone()), net - net.signum() * count);
    st.seq += 1;
    let id = format!("ord_close_{}", st.seq);
    let order = json!({"order_id": id, "ticker": ticker, "status": "FILLED", "count": count, "filled_count": count, "remaining_count": 0});
    Json(json!({
        "order": order, "fills": [{"count": count, "price_ticks": 50, "fee_micro_usdc": 0}],
        "reject_code": "", "reject_reason": "",
        "position_before": net, "closing": {"side": "YES", "action": "SELL", "count": count}
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// account
// ---------------------------------------------------------------------------

async fn cancel_after_get(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let st = gw.state.lock().unwrap();
    let v = st
        .extra
        .get(&format!("cancel_after:{user}"))
        .cloned()
        .unwrap_or_else(
            || json!({"armed": false, "seconds": 0, "cancel_at": null, "armed_at": null}),
        );
    Json(v).into_response()
}

async fn cancel_after_set(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let user = match trader(&gw, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let seconds = body["seconds"].as_i64().unwrap_or(-1);
    if !(0..=3600).contains(&seconds) {
        return err(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "seconds must be 0 (off) or 1 to 3600",
        );
    }
    let v = if seconds == 0 {
        json!({"armed": false, "seconds": 0, "cancel_at": null, "armed_at": null})
    } else {
        json!({"armed": true, "seconds": seconds, "cancel_at": "2026-11-12T19:05:00Z", "armed_at": "2026-11-12T19:00:00Z"})
    };
    gw.state
        .lock()
        .unwrap()
        .extra
        .insert(format!("cancel_after:{user}"), v.clone());
    Json(v).into_response()
}

async fn account_fees(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    if let Err(r) = auth(&gw, &headers) {
        return r;
    }
    Json(json!({"window_days": 30, "maker_contracts": 12, "taker_contracts": 30, "fees_paid_micro_usdc": 0,
        "schedule": {"taker_fee_bps": 0, "maker_fee_bps": 0, "maker_incentive_bps": 0}}))
    .into_response()
}

async fn account_incentives(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    if let Err(r) = auth(&gw, &headers) {
        return r;
    }
    Json(json!({"incentives": [], "incentive_bps": 0})).into_response()
}

async fn account_settlements(State(gw): State<MockGateway>, headers: HeaderMap) -> Response {
    if let Err(r) = auth(&gw, &headers) {
        return r;
    }
    Json(json!({"settlements": [], "next_cursor": ""})).into_response()
}

async fn account_activity(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (user, _) = match auth(&gw, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let types = q
        .get("types")
        .map(String::as_str)
        .unwrap_or("fill,order,settlement,cash");
    for t in types.split(',') {
        if !["fill", "order", "settlement", "cash"].contains(&t.trim()) {
            return err(
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                &format!("unknown activity type '{t}'; use fill, order, settlement, cash"),
            );
        }
    }
    let st = gw.state.lock().unwrap();
    let mut items = Vec::new();
    if types.contains("order") {
        for o in st.orders.values().filter(|o| o["user_id"] == user) {
            items.push(json!({"type": "order", "id": o["order_id"], "ts": "2026-11-12T19:00:00Z", "ticker": o["ticker"],
                "status": o["status"], "side": o["side"], "action": o["action"], "price_ticks": o["price_ticks"],
                "count": o["count"], "filled_count": o["filled_count"]}));
        }
    }
    if types.contains("cash") {
        items.push(json!({"type": "cash", "id": "1", "ts": "2026-11-12T18:00:00Z", "ticker": null,
            "reason_code": "DEMO_DEPOSIT", "direction": "CREDIT", "amount_micro_usdc": 100_000_000}));
    }
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 200);
    items.truncate(limit);
    Json(json!({"count": items.len(), "types": types.split(',').collect::<Vec<_>>(), "activity": items, "next_to": null}))
        .into_response()
}

async fn patch_limits(
    State(gw): State<MockGateway>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = auth(&gw, &headers) {
        return r;
    }
    Json(json!({"key_id": key_id, "limits": body["limits"]})).into_response()
}
