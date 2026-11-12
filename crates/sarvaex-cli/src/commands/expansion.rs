//! Commands for the gateway's expanded API: amend, batch and bulk cancel, close
//! position, dead man's switch, candles/trades/stats, fees, activity feed,
//! settlements, exchange status and per-key trading caps.
//!
//! Responses are kept as JSON values (the gateway may add fields), and the human
//! view picks out the fields that matter. Every command also supports `--json`.

use super::Ctx;
use crate::cli::{CancelAfterCmd, ExchangeCmd, LimitsArgs};
use crate::output::{exit, kv, short_ts, table, truncate};
use anyhow::{Context, Result, anyhow, bail};
use sarvaex_sdk::price::PriceEncoding;
use sarvaex_sdk::{Action, NewOrder, Side, Tif};
use serde_json::{Map, Value, json};
use std::io::Read;

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn s(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::Null => String::new(),
        Value::String(x) => x.clone(),
        other => other.to_string(),
    }
}

fn i(v: &Value, key: &str) -> Option<i64> {
    v[key].as_i64()
}

fn ticks(enc: &PriceEncoding, v: &Value, key: &str) -> String {
    i(v, key).map(|t| enc.format(t)).unwrap_or_default()
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn usdc_to_micro(usdc: f64) -> i64 {
    (usdc * 1_000_000.0).round() as i64
}

/// Build the gateway's `limits` object from flags. `None` when nothing was asked for.
pub fn limits_value(a: &LimitsArgs) -> Result<Option<Value>> {
    if let Some(raw) = &a.limits_json {
        let v: Value = serde_json::from_str(raw).context("--limits-json is not valid JSON")?;
        if !v.is_object() {
            bail!("--limits-json must be a JSON object");
        }
        return Ok(Some(v));
    }
    let mut o = Map::new();
    if a.read_only {
        o.insert("read_only".into(), json!(true));
    }
    if let Some(n) = a.max_order_count {
        if n <= 0 {
            bail!("--max-order-count must be positive");
        }
        o.insert("max_order_count".into(), json!(n));
    }
    if let Some(u) = a.max_order_notional_usdc {
        if u <= 0.0 {
            bail!("--max-order-notional-usdc must be positive");
        }
        o.insert(
            "max_order_notional_micro_usdc".into(),
            json!(usdc_to_micro(u)),
        );
    }
    if let Some(u) = a.max_daily_notional_usdc {
        if u <= 0.0 {
            bail!("--max-daily-notional-usdc must be positive");
        }
        o.insert(
            "max_daily_notional_micro_usdc".into(),
            json!(usdc_to_micro(u)),
        );
    }
    if !a.allow_ticker.is_empty() {
        let t: Vec<String> = a
            .allow_ticker
            .iter()
            .map(|t| t.trim().to_ascii_uppercase())
            .collect();
        o.insert("allowed_tickers".into(), json!(t));
    }
    if !a.allow_event.is_empty() {
        let t: Vec<String> = a
            .allow_event
            .iter()
            .map(|t| t.trim().to_ascii_uppercase())
            .collect();
        o.insert("allowed_event_tickers".into(), json!(t));
    }
    if !a.ip_allow.is_empty() {
        o.insert("ip_allowlist".into(), json!(a.ip_allow));
    }
    Ok((!o.is_empty()).then_some(Value::Object(o)))
}

// ---------------------------------------------------------------------------
// exchange
// ---------------------------------------------------------------------------

pub async fn exchange(ctx: &Ctx, cmd: ExchangeCmd) -> Result<i32> {
    match cmd {
        ExchangeCmd::Status => {
            let v = ctx.client().exchange_status().await?;
            ctx.out.emit(&v, || status_text(&v));
        }
        ExchangeCmd::Schedule => {
            let v = ctx.client().exchange_schedule().await?;
            ctx.out.emit(&v, || {
                let mut out = status_text(&v["status"]);
                let rows = v["contracts_closing_next_7d"].as_array();
                match rows {
                    Some(rows) if !rows.is_empty() => {
                        let mut t = table(&["ticker", "closes", "state"]);
                        for c in rows {
                            t.add_row(vec![
                                s(c, "ticker"),
                                short_ts(&c["close_at"].as_str().map(str::to_owned)),
                                s(c, "state"),
                            ]);
                        }
                        out.push_str(&format!("\n\nClosing in the next 7 days:\n{t}"));
                    }
                    _ => out.push_str("\n\nNo contracts closing in the next 7 days."),
                }
                out
            });
        }
        ExchangeCmd::Time => {
            let v = ctx.client().server_time().await?;
            ctx.out.emit(&v, || {
                format!("{}  (epoch {} ms)", s(&v, "server_time"), s(&v, "epoch_ms"))
            });
        }
        ExchangeCmd::Fees => {
            let v = ctx.client().fee_schedule().await?;
            ctx.out.emit(&v, || {
                let bps = |k: &str| format!("{} bps", s(&v, k));
                format!(
                    "{}\n\n{}",
                    kv(&[
                        ("taker fee", bps("taker_fee_bps")),
                        ("maker fee", bps("maker_fee_bps")),
                        ("maker incentive", bps("maker_incentive_bps")),
                    ]),
                    s(&v, "fee_basis")
                )
            });
        }
    }
    Ok(exit::OK)
}

fn status_text(v: &Value) -> String {
    let yes = |k: &str| {
        if v[k].as_bool().unwrap_or(false) {
            "yes"
        } else {
            "no"
        }
    };
    let mut rows = vec![
        ("mode", s(v, "mode")),
        ("new orders allowed", yes("new_orders_allowed").to_owned()),
        ("cancels allowed", yes("cancels_allowed").to_owned()),
        ("in maintenance", yes("in_maintenance").to_owned()),
    ];
    if let Some(from) = v["maintenance_starts_at"].as_str() {
        rows.push(("maintenance from", from.to_owned()));
    }
    if let Some(to) = v["maintenance_ends_at"].as_str() {
        rows.push(("maintenance until", to.to_owned()));
    }
    let message = s(v, "message");
    if !message.is_empty() {
        rows.push(("message", message));
    }
    kv(&rows)
}

// ---------------------------------------------------------------------------
// orders: amend, batch, cancel-batch
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn amend(
    ctx: &Ctx,
    order_id: String,
    price: Option<String>,
    price_ticks: Option<i64>,
    count: Option<i64>,
    no_cancel_replace: bool,
    idempotency_key: Option<String>,
) -> Result<i32> {
    let new_ticks = match (price_ticks, price) {
        (Some(t), _) => Some(t),
        (None, Some(p)) => {
            let order = ctx.client().get_order(&order_id).await?;
            let enc = ctx.encoding(&order.ticker).await;
            Some(enc.parse(&p)?)
        }
        (None, None) => None,
    };
    if new_ticks.is_none() && count.is_none() {
        bail!("give --price (or --price-ticks) and/or --count");
    }
    let v = ctx
        .client()
        .amend_order(
            &order_id,
            new_ticks,
            count,
            !no_cancel_replace,
            idempotency_key.as_deref(),
        )
        .await?;
    let reject = s(&v, "reject_code");
    ctx.out.emit(&v, || {
        let o = if v["order"].is_object() { &v["order"] } else { &v };
        if !reject.is_empty() {
            return format!("amend rejected: {} {}", reject, s(&v, "reject_reason"));
        }
        let how = match s(&v, "amended_via").as_str() {
            "IN_PLACE" => "amended in place (keeps its place in the queue)".to_owned(),
            "CANCEL_REPLACE" => format!(
                "replaced: the old order was cancelled and a new one placed (queue priority lost)\nnew order {}",
                s(o, "order_id")
            ),
            other => other.to_owned(),
        };
        format!(
            "{how}\nprice {} ticks · count {} · status {}",
            s(o, "price_ticks"),
            s(o, "count"),
            s(o, "status")
        )
    });
    Ok(if reject.is_empty() {
        exit::OK
    } else {
        exit::REJECTED
    })
}

fn parse_side(raw: &str) -> Result<Side> {
    serde_json::from_value(json!(raw.trim().to_ascii_uppercase()))
        .map_err(|_| anyhow!("side must be yes, no, long or short (got '{raw}')"))
}

fn parse_action(raw: &str) -> Result<Action> {
    serde_json::from_value(json!(raw.trim().to_ascii_uppercase()))
        .map_err(|_| anyhow!("action must be buy or sell (got '{raw}')"))
}

fn parse_tif(raw: &str) -> Result<Tif> {
    serde_json::from_value(json!(raw.trim().to_ascii_uppercase()))
        .map_err(|_| anyhow!("tif must be gtc, ioc, fok or day (got '{raw}')"))
}

/// Turn one entry of a batch file into an order, converting human prices to ticks.
async fn batch_entry(ctx: &Ctx, n: usize, e: &Value) -> Result<NewOrder> {
    let field = |k: &str| e[k].as_str().map(str::to_owned);
    let ticker = field("ticker").ok_or_else(|| anyhow!("order {n}: missing ticker"))?;
    let side = parse_side(&field("side").ok_or_else(|| anyhow!("order {n}: missing side"))?)?;
    let action =
        parse_action(&field("action").ok_or_else(|| anyhow!("order {n}: missing action"))?)?;
    let count = e["count"]
        .as_i64()
        .filter(|c| *c > 0)
        .ok_or_else(|| anyhow!("order {n}: count must be a positive integer"))?;
    let enc = ctx.encoding(&ticker).await;
    let price_ticks = match (
        e["price_ticks"].as_i64(),
        e["price"].as_str(),
        e["price"].as_f64(),
    ) {
        (Some(t), _, _) => t,
        (None, Some(p), _) => enc.parse(p)?,
        (None, None, Some(p)) => enc.parse(&p.to_string())?,
        _ => bail!("order {n}: give price or price_ticks"),
    };
    let mut o = NewOrder::limit(
        ticker.trim().to_ascii_uppercase(),
        side,
        action,
        price_ticks,
        count,
    );
    if let Some(t) = field("tif") {
        o.tif = parse_tif(&t)?;
    }
    o.post_only = e["post_only"].as_bool().unwrap_or(false);
    o.reduce_only = e["reduce_only"].as_bool().unwrap_or(false);
    if let Some(id) = field("client_order_id") {
        o.client_order_id = id;
    }
    o.expires_at = field("expires_at");
    Ok(o)
}

pub async fn batch(ctx: &Ctx, file: String, idempotency_key: Option<String>) -> Result<i32> {
    let raw = if file == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading stdin")?;
        buf
    } else {
        std::fs::read_to_string(&file).with_context(|| format!("reading {file}"))?
    };
    let entries: Value = serde_json::from_str(&raw).context("batch file is not valid JSON")?;
    let entries = entries
        .as_array()
        .ok_or_else(|| anyhow!("batch file must be a JSON array of orders"))?;
    if entries.is_empty() || entries.len() > 20 {
        bail!("a batch holds 1 to 20 orders (got {})", entries.len());
    }
    let mut orders = Vec::with_capacity(entries.len());
    for (n, e) in entries.iter().enumerate() {
        let order = batch_entry(ctx, n + 1, e).await?;
        // Same local checks and safety guards as a single order.
        ctx.client().validate_order(&order).await?;
        orders.push(order);
    }
    if !ctx.confirm(&format!("Place {} order(s)?", orders.len())) {
        bail!("aborted");
    }
    let v = ctx
        .client()
        .batch_orders(&orders, idempotency_key.as_deref())
        .await?;
    let failed = i(&v, "failed").unwrap_or(0);
    ctx.out.emit(&v, || {
        let mut t = table(&["#", "ticker", "result", "order id"]);
        for r in v["results"].as_array().into_iter().flatten() {
            let body = &r["body"];
            let o = if body["order"].is_object() {
                &body["order"]
            } else {
                body
            };
            let reject = s(body, "reject_code");
            let result = if !reject.is_empty() {
                format!("rejected {reject}")
            } else if body["error"].is_object() {
                format!("error {}", s(&body["error"], "code"))
            } else {
                s(o, "status")
            };
            t.add_row(vec![
                s(r, "index"),
                truncate(&s(o, "ticker"), 28),
                result,
                s(o, "order_id"),
            ]);
        }
        format!(
            "accepted {} · failed {}\n{t}",
            i(&v, "accepted").unwrap_or(0),
            failed
        )
    });
    Ok(if failed == 0 {
        exit::OK
    } else {
        exit::REJECTED
    })
}

pub async fn cancel_batch(
    ctx: &Ctx,
    order_ids: Vec<String>,
    idempotency_key: Option<String>,
) -> Result<i32> {
    let v = ctx
        .client()
        .cancel_batch(&order_ids, idempotency_key.as_deref())
        .await?;
    let failed = v["failed"].as_array().map(Vec::len).unwrap_or(0);
    ctx.out.emit(&v, || {
        let mut out = format!(
            "cancelled {} order(s)",
            v["cancelled"].as_array().map(Vec::len).unwrap_or(0)
        );
        for f in v["failed"].as_array().into_iter().flatten() {
            out.push_str(&format!(
                "\nfailed {}: {} {}",
                s(f, "order_id"),
                s(f, "reject_code"),
                s(f, "reject_reason")
            ));
        }
        out
    });
    Ok(if failed == 0 { exit::OK } else { exit::ERROR })
}

// ---------------------------------------------------------------------------
// positions: close
// ---------------------------------------------------------------------------

pub async fn close_position(
    ctx: &Ctx,
    ticker: String,
    count: Option<i64>,
    price: Option<String>,
    price_ticks: Option<i64>,
    idempotency_key: Option<String>,
) -> Result<i32> {
    let contract = ctx.contract(&ticker).await?;
    let enc = PriceEncoding::for_contract(&contract);
    let limit = match (price_ticks, price) {
        (Some(t), _) => Some(t),
        (None, Some(p)) => Some(enc.parse(&p)?),
        (None, None) => None,
    };
    if !ctx.confirm(&format!(
        "Close {} on {}?",
        count
            .map(|c| format!("{c} contract(s)"))
            .unwrap_or_else(|| "the whole position".into()),
        contract.ticker
    )) {
        bail!("aborted");
    }
    let v = ctx
        .client()
        .close_position(&contract.ticker, count, limit, idempotency_key.as_deref())
        .await?;
    let reject = s(&v, "reject_code");
    ctx.out.emit(&v, || {
        if !reject.is_empty() {
            return format!("close rejected: {reject} {}", s(&v, "reject_reason"));
        }
        let o = &v["order"];
        let filled: i64 = v["fills"]
            .as_array()
            .map(|f| f.iter().filter_map(|x| x["count"].as_i64()).sum())
            .unwrap_or(0);
        format!(
            "closing {} {} on {}\norder {} · {} · filled {} contract(s)",
            s(&v["closing"], "action"),
            s(&v["closing"], "count"),
            contract.ticker,
            s(o, "order_id"),
            s(o, "status"),
            filled
        )
    });
    Ok(if reject.is_empty() {
        exit::OK
    } else {
        exit::REJECTED
    })
}

// ---------------------------------------------------------------------------
// markets: candles, trades, stats, books
// ---------------------------------------------------------------------------

pub async fn candles(
    ctx: &Ctx,
    ticker: String,
    interval: String,
    from: Option<String>,
    to: Option<String>,
    limit: u32,
) -> Result<i32> {
    let enc = ctx.encoding(&ticker).await;
    let v = ctx
        .client()
        .candles(
            &ticker.trim().to_ascii_uppercase(),
            &interval,
            from.as_deref(),
            to.as_deref(),
            Some(limit.clamp(1, 1000)),
        )
        .await?;
    ctx.out.emit(&v, || {
        let rows = v["candles"].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            return format!("no trades in this range ({interval} candles omit empty buckets)");
        }
        let mut t = table(&["time", "open", "high", "low", "close", "volume", "trades"]);
        for c in &rows {
            t.add_row(vec![
                short_ts(&c["ts"].as_str().map(str::to_owned)),
                ticks(&enc, c, "open"),
                ticks(&enc, c, "high"),
                ticks(&enc, c, "low"),
                ticks(&enc, c, "close"),
                s(c, "volume"),
                s(c, "trades"),
            ]);
        }
        t.to_string()
    });
    Ok(exit::OK)
}

pub async fn trades(
    ctx: &Ctx,
    ticker: String,
    from: Option<String>,
    to: Option<String>,
    limit: u32,
) -> Result<i32> {
    let enc = ctx.encoding(&ticker).await;
    let v = ctx
        .client()
        .market_trades(
            &ticker.trim().to_ascii_uppercase(),
            from.as_deref(),
            to.as_deref(),
            Some(limit.clamp(1, 1000)),
        )
        .await?;
    ctx.out.emit(&v, || {
        let rows = v["trades"].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            return "no trades in this range".to_owned();
        }
        let mut t = table(&["time", "price", "count", "aggressor"]);
        for r in &rows {
            t.add_row(vec![
                short_ts(&r["ts"].as_str().map(str::to_owned)),
                ticks(&enc, r, "price_ticks"),
                s(r, "count"),
                s(r, "aggressor_side"),
            ]);
        }
        t.to_string()
    });
    Ok(exit::OK)
}

pub async fn stats(ctx: &Ctx, tickers: Vec<String>) -> Result<i32> {
    let tickers: Vec<String> = tickers
        .iter()
        .map(|t| t.trim().to_ascii_uppercase())
        .collect();
    if tickers.len() == 1 {
        let enc = ctx.encoding(&tickers[0]).await;
        let v = ctx.client().market_stats(&tickers[0]).await?;
        ctx.out.emit(&v, || {
            kv(&[
                ("ticker", s(&v, "ticker")),
                ("last", ticks(&enc, &v, "last_price_ticks")),
                ("last trade", s(&v, "last_trade_ts")),
                ("24h change", s(&v, "price_change_24h_ticks")),
                ("24h high", ticks(&enc, &v, "high_24h_ticks")),
                ("24h low", ticks(&enc, &v, "low_24h_ticks")),
                ("24h volume", s(&v, "volume_24h")),
                ("24h trades", s(&v, "trades_24h")),
                ("best bid", s(&v["best_bid"], "price_ticks")),
                ("best ask", s(&v["best_ask"], "price_ticks")),
                ("open interest", s(&v, "open_interest")),
            ])
        });
    } else {
        let v = ctx.client().markets_stats(&tickers).await?;
        ctx.out.emit(&v, || {
            let mut t = table(&[
                "ticker",
                "last (ticks)",
                "24h volume",
                "24h trades",
                "24h change",
            ]);
            for m in v["stats"].as_array().into_iter().flatten() {
                t.add_row(vec![
                    truncate(&s(m, "ticker"), 34),
                    s(m, "last_price_ticks"),
                    s(m, "volume_24h"),
                    s(m, "trades_24h"),
                    s(m, "price_change_24h_ticks"),
                ]);
            }
            t.to_string()
        });
    }
    Ok(exit::OK)
}

pub async fn books(ctx: &Ctx, tickers: Vec<String>, depth: u32) -> Result<i32> {
    let tickers: Vec<String> = tickers
        .iter()
        .map(|t| t.trim().to_ascii_uppercase())
        .collect();
    let v = ctx
        .client()
        .orderbooks(&tickers, Some(depth.clamp(1, 100)))
        .await?;
    ctx.out.emit(&v, || {
        let mut t = table(&["ticker", "yes bid", "yes ask", "no bid", "no ask"]);
        for b in v["orderbooks"].as_array().into_iter().flatten() {
            let best = &b["best"];
            t.add_row(vec![
                truncate(&s(b, "ticker"), 34),
                s(best, "yes_bid"),
                s(best, "yes_ask"),
                s(best, "no_bid"),
                s(best, "no_ask"),
            ]);
        }
        let mut out = t.to_string();
        for e in v["errors"].as_array().into_iter().flatten() {
            out.push_str(&format!("\n{}: {}", s(e, "ticker"), s(e, "code")));
        }
        out
    });
    Ok(exit::OK)
}

// ---------------------------------------------------------------------------
// account: fees, activity, settlements, incentives, cancel-after
// ---------------------------------------------------------------------------

pub async fn account_fees(ctx: &Ctx) -> Result<i32> {
    let v = ctx.client().account_fees().await?;
    ctx.out.emit(&v, || pretty(&v));
    Ok(exit::OK)
}

pub async fn incentives(ctx: &Ctx) -> Result<i32> {
    let v = ctx.client().account_incentives().await?;
    ctx.out.emit(&v, || pretty(&v));
    Ok(exit::OK)
}

pub async fn activity(
    ctx: &Ctx,
    types: Option<String>,
    from: Option<String>,
    to: Option<String>,
    ticker: Option<String>,
    limit: u32,
) -> Result<i32> {
    let v = ctx
        .client()
        .account_activity(
            types.as_deref(),
            from.as_deref(),
            to.as_deref(),
            ticker.as_deref(),
            Some(limit.clamp(1, 200)),
        )
        .await?;
    ctx.out.emit(&v, || {
        let rows = v["activity"].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            return "no activity in this range".to_owned();
        }
        let mut t = table(&["time", "type", "ticker", "detail"]);
        for r in &rows {
            let detail = match s(r, "type").as_str() {
                "fill" => format!(
                    "{} {} {} {} @ {}",
                    s(r, "role"),
                    s(r, "action"),
                    s(r, "count"),
                    s(r, "side"),
                    s(r, "price_ticks")
                ),
                "order" => format!(
                    "{} {} {} {} @ {} filled {}/{}",
                    s(r, "status"),
                    s(r, "action"),
                    s(r, "count"),
                    s(r, "side"),
                    s(r, "price_ticks"),
                    s(r, "filled_count"),
                    s(r, "count")
                ),
                "settlement" => format!(
                    "payout {} micro-USDC ({})",
                    s(r, "payout_micro_usdc"),
                    s(r, "status")
                ),
                "cash" => format!(
                    "{} {} micro-USDC ({})",
                    s(r, "direction"),
                    s(r, "amount_micro_usdc"),
                    s(r, "reason_code")
                ),
                _ => String::new(),
            };
            t.add_row(vec![
                short_ts(&r["ts"].as_str().map(str::to_owned)),
                s(r, "type"),
                truncate(&s(r, "ticker"), 30),
                detail,
            ]);
        }
        let mut out = t.to_string();
        if let Some(next) = v["next_to"].as_str() {
            out.push_str(&format!("\nmore: --to {next}"));
        }
        out
    });
    Ok(exit::OK)
}

pub async fn settlements(ctx: &Ctx, ticker: Option<String>, limit: u32) -> Result<i32> {
    let v = ctx
        .client()
        .account_settlements(ticker.as_deref(), Some(limit.clamp(1, 200)))
        .await?;
    ctx.out.emit(&v, || {
        let rows = v["settlements"]
            .as_array()
            .or_else(|| v["payouts"].as_array())
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            return "no settlement payouts yet".to_owned();
        }
        let mut t = table(&[
            "ticker",
            "position",
            "payout (micro-USDC)",
            "status",
            "posted",
        ]);
        for r in &rows {
            t.add_row(vec![
                truncate(&s(r, "ticker"), 34),
                s(r, "position_qty"),
                s(r, "payout_micro_usdc"),
                s(r, "status"),
                short_ts(&r["posted_at"].as_str().map(str::to_owned)),
            ]);
        }
        t.to_string()
    });
    Ok(exit::OK)
}

pub async fn cancel_after(ctx: &Ctx, cmd: CancelAfterCmd) -> Result<i32> {
    let v = match cmd {
        CancelAfterCmd::Status => ctx.client().cancel_after_status().await?,
        CancelAfterCmd::Set { seconds } => {
            if seconds == 0 {
                bail!("use `cancel-after off` to disarm");
            }
            ctx.client().set_cancel_after(seconds, None).await?
        }
        CancelAfterCmd::Off => ctx.client().set_cancel_after(0, None).await?,
    };
    ctx.out.emit(&v, || {
        if v["armed"].as_bool().unwrap_or(false) {
            format!(
                "ARMED: every working order is cancelled at {} unless refreshed\n(placing an order or running `cancel-after set` again pushes it out by {} s)",
                s(&v, "cancel_at"),
                s(&v, "seconds")
            )
        } else {
            "not armed".to_owned()
        }
    });
    Ok(exit::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_flags_means_no_limits() {
        assert!(limits_value(&LimitsArgs::default()).unwrap().is_none());
    }

    #[test]
    fn flags_become_the_gateway_limits_object() {
        let a = LimitsArgs {
            read_only: true,
            max_order_count: Some(5),
            max_order_notional_usdc: Some(25.5),
            max_daily_notional_usdc: Some(500.0),
            allow_ticker: vec![" sx-a ".into()],
            allow_event: vec!["xev-1".into()],
            ip_allow: vec!["203.0.113.7".into()],
            limits_json: None,
        };
        let v = limits_value(&a).unwrap().unwrap();
        assert_eq!(v["read_only"], true);
        assert_eq!(v["max_order_count"], 5);
        assert_eq!(v["max_order_notional_micro_usdc"], 25_500_000);
        assert_eq!(v["max_daily_notional_micro_usdc"], 500_000_000);
        assert_eq!(v["allowed_tickers"][0], "SX-A");
        assert_eq!(v["allowed_event_tickers"][0], "XEV-1");
        assert_eq!(v["ip_allowlist"][0], "203.0.113.7");
    }

    #[test]
    fn raw_json_wins_and_must_be_an_object() {
        let a = LimitsArgs {
            max_order_count: Some(1),
            limits_json: Some(r#"{"max_order_count": 9}"#.into()),
            ..Default::default()
        };
        assert_eq!(limits_value(&a).unwrap().unwrap()["max_order_count"], 9);
        let bad = LimitsArgs {
            limits_json: Some("[1]".into()),
            ..Default::default()
        };
        assert!(limits_value(&bad).is_err());
        let bad = LimitsArgs {
            limits_json: Some("nope".into()),
            ..Default::default()
        };
        assert!(limits_value(&bad).is_err());
    }

    #[test]
    fn nonsense_limits_are_refused_locally() {
        for a in [
            LimitsArgs {
                max_order_count: Some(0),
                ..Default::default()
            },
            LimitsArgs {
                max_order_notional_usdc: Some(-1.0),
                ..Default::default()
            },
            LimitsArgs {
                max_daily_notional_usdc: Some(0.0),
                ..Default::default()
            },
        ] {
            assert!(limits_value(&a).is_err());
        }
    }

    #[test]
    fn side_action_and_tif_parse_case_insensitively() {
        assert!(matches!(parse_side("Yes").unwrap(), Side::Yes));
        assert!(matches!(parse_side(" short ").unwrap(), Side::Short));
        assert!(parse_side("up").is_err());
        assert!(matches!(parse_action("SELL").unwrap(), Action::Sell));
        assert!(parse_action("hold").is_err());
        assert!(matches!(parse_tif("ioc").unwrap(), Tif::Ioc));
        assert!(parse_tif("forever").is_err());
    }
}
