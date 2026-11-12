use super::Ctx;
use crate::cli::{OrdersCmd, PlaceArgs};
use crate::output::{exit, short_ts, table};
use anyhow::{Result, anyhow, bail};
use sarvaex_sdk::idempotency;
use sarvaex_sdk::price::{self, PriceEncoding};
use sarvaex_sdk::{NewOrder, Order, OrderFilter, OrderResponse};
use serde_json::json;
use std::collections::HashMap;

pub async fn run(ctx: &Ctx, cmd: OrdersCmd) -> Result<i32> {
    match cmd {
        OrdersCmd::Place(args) => place(ctx, args).await,
        OrdersCmd::List {
            ticker,
            status,
            limit,
            cursor,
            all,
        } => list(ctx, ticker, &status, limit, cursor, all).await,
        OrdersCmd::Get { order_id } => {
            let o = ctx.client().get_order(&order_id).await?;
            let enc = ctx.encoding(&o.ticker).await;
            ctx.out.emit(&o, || order_detail(&o, &enc));
            Ok(exit::OK)
        }
        OrdersCmd::Cancel {
            order_id,
            idempotency_key,
        } => {
            let resp = ctx
                .client()
                .cancel_order(&order_id, idempotency_key.as_deref())
                .await?;
            emit_order_response(ctx, &resp, None).await
        }
        OrdersCmd::Amend {
            order_id,
            price,
            price_ticks,
            count,
            no_cancel_replace,
            idempotency_key,
        } => {
            super::expansion::amend(
                ctx,
                order_id,
                price,
                price_ticks,
                count,
                no_cancel_replace,
                idempotency_key,
            )
            .await
        }
        OrdersCmd::Batch {
            file,
            idempotency_key,
        } => super::expansion::batch(ctx, file, idempotency_key).await,
        OrdersCmd::CancelBatch {
            order_ids,
            idempotency_key,
        } => super::expansion::cancel_batch(ctx, order_ids, idempotency_key).await,
        OrdersCmd::CancelAll { ticker } => {
            if !ctx.confirm(&format!(
                "Cancel ALL working orders{}?",
                ticker
                    .as_deref()
                    .map(|t| format!(" on {t}"))
                    .unwrap_or_default()
            )) {
                bail!("aborted");
            }
            let (cancelled, failed) = ctx.client().cancel_all_orders(ticker.as_deref()).await?;
            ctx.out.emit(
                &json!({ "cancelled": cancelled, "failed": failed.iter().map(|(id, why)| json!({"order_id": id, "reason": why})).collect::<Vec<_>>() }),
                || {
                    let mut s = format!("cancelled {} order(s)", cancelled.len());
                    for (id, why) in &failed {
                        s.push_str(&format!("\nfailed {id}: {why}"));
                    }
                    s
                },
            );
            Ok(if failed.is_empty() {
                exit::OK
            } else {
                exit::ERROR
            })
        }
    }
}

/// Resolve a human price (`--price`) or raw ticks into ticks.
pub fn resolve_price(enc: &PriceEncoding, price: Option<&str>, ticks: Option<i64>) -> Result<i64> {
    match (ticks, price) {
        (Some(t), _) => Ok(t),
        (None, Some(p)) => Ok(enc.parse(p)?),
        (None, None) => Err(anyhow!("provide --price, --price-ticks, or --market")),
    }
}

async fn place(ctx: &Ctx, args: PlaceArgs) -> Result<i32> {
    let contract = ctx.contract(&args.ticker).await?;
    let enc = PriceEncoding::for_contract(&contract);
    let side = args.side.into();
    let action = args.action.into();

    let requested: Option<sarvaex_sdk::OrderType> = args.order_type.map(Into::into);
    let trigger = requested.filter(|t| t.is_trigger());
    if trigger.is_some() && args.market {
        bail!("--market cannot be combined with a stop or take-profit --type");
    }
    if let Some(t) = requested {
        if t == sarvaex_sdk::OrderType::Market && !args.market && trigger.is_none() {
            bail!("--type market is the same as --market; use --market");
        }
        if t == sarvaex_sdk::OrderType::Limit && args.market {
            bail!("--type limit conflicts with --market");
        }
    }
    if trigger.is_none() && (args.trigger_price.is_some() || args.trigger_price_ticks.is_some()) {
        bail!(
            "--trigger-price needs --type stop-market, stop-limit, take-profit or take-profit-limit"
        );
    }
    let mut order = if let Some(kind) = trigger {
        let trigger_ticks = match (args.trigger_price_ticks, args.trigger_price.as_deref()) {
            (Some(t), _) => t,
            (None, Some(p)) => enc.parse(p)?,
            (None, None) => bail!(
                "give --trigger-price (or --trigger-price-ticks) for a stop or take-profit order"
            ),
        };
        let limit = if kind.has_limit_price() {
            Some(resolve_price(
                &enc,
                args.price.as_deref(),
                args.price_ticks,
            )?)
        } else {
            if args.price.is_some() || args.price_ticks.is_some() {
                bail!(
                    "{} places a market order when it fires and takes no --price; use a *-limit type for a limit price",
                    kind.as_str()
                );
            }
            None
        };
        NewOrder::trigger(
            contract.ticker.clone(),
            side,
            action,
            kind,
            trigger_ticks,
            limit,
            args.count,
        )
    } else if args.market {
        NewOrder::market(contract.ticker.clone(), side, action, args.count)
    } else {
        let ticks = resolve_price(&enc, args.price.as_deref(), args.price_ticks)?;
        NewOrder::limit(contract.ticker.clone(), side, action, ticks, args.count)
    };
    if !order.order_type.is_market_style() {
        order.tif = args.tif.into();
    }
    order.post_only = args.post_only;
    // A stop or take-profit protects a position, so it only reduces one unless told otherwise.
    order.reduce_only = if trigger.is_some() {
        !args.allow_open
    } else {
        args.reduce_only
    };
    if let Some(s) = args.stp {
        order.stp = s.into();
    }
    order.expires_at = args.expires_at.clone();
    if let Some(id) = &args.client_order_id {
        order.client_order_id = id.clone();
    }
    let idem = args.idempotency_key.clone().or_else(|| {
        args.client_order_id
            .as_deref()
            .map(|id| idempotency::idempotency_key_for("order", id))
    });

    // Local validation + guards before anything is sent.
    ctx.client().validate_order(&order).await?;

    let market_style = order.order_type.is_market_style();
    let display_price = if market_style {
        "MARKET".to_owned()
    } else {
        enc.format(order.price_ticks)
    };
    let trigger_note = order
        .trigger_price_ticks
        .map(|t| {
            format!(
                ", {} when the last price reaches {}",
                order.order_type.as_str(),
                enc.format(t)
            )
        })
        .unwrap_or_default();
    let notional = if market_style {
        None
    } else {
        price::estimated_notional_micro(&contract, order.price_ticks, order.count)
            .map(price::micro_to_usdc)
    };
    let describe = format!(
        "{} {} {} × {} @ {}{}{}  [{}{}{}]",
        action,
        order.count,
        side,
        contract.ticker,
        display_price,
        notional
            .as_ref()
            .map(|n| format!(" (≈ {n} USDC)"))
            .unwrap_or_default(),
        trigger_note,
        order.tif.as_str(),
        if order.post_only { ", post-only" } else { "" },
        if order.reduce_only {
            ", reduce-only"
        } else {
            ""
        },
    );

    if args.dry_run {
        ctx.out.emit(
            &json!({ "dry_run": true, "would_send": order, "idempotency_key": idem, "display": { "price": display_price, "estimated_notional_usdc": notional, "summary": describe } }),
            || format!("DRY RUN — would send: {describe}\n{}", serde_json::to_string_pretty(&order).unwrap_or_default()),
        );
        return Ok(exit::OK);
    }

    if !ctx.confirm(&format!("Place order: {describe}?")) {
        bail!("aborted");
    }

    let resp = ctx
        .client()
        .place_order_unchecked(&order, idem.as_deref())
        .await?;
    emit_order_response(ctx, &resp, Some(&enc)).await
}

async fn emit_order_response(
    ctx: &Ctx,
    resp: &OrderResponse,
    enc: Option<&PriceEncoding>,
) -> Result<i32> {
    let enc = match enc {
        Some(e) => e.clone(),
        None if !resp.order.ticker.is_empty() => ctx.encoding(&resp.order.ticker).await,
        None => PriceEncoding::binary(),
    };
    let rejected = resp.is_rejected();
    let mut v = serde_json::to_value(resp)?;
    v["rejected"] = json!(rejected);
    v["summary"] = json!(resp.summary());
    if resp.order.avg_fill_price_ticks > 0 {
        v["display"] = json!({ "avg_fill_price": enc.format(resp.order.avg_fill_price_ticks), "price": enc.format(resp.order.price_ticks) });
    }
    ctx.out.emit(&v, || {
        let mut s = format!("{}  {}", if rejected { "✗" } else { "✓" }, resp.summary());
        if !resp.order.order_id.is_empty() {
            s.push_str(&format!("\norder_id: {}", resp.order.order_id));
        }
        if !resp.fills.is_empty() {
            s.push_str("\nfills:");
            for f in &resp.fills {
                s.push_str(&format!(
                    "\n  {} × {} ({})",
                    f.count,
                    enc.format(f.price_ticks),
                    f.fill_id
                ));
            }
        }
        s
    });
    Ok(if rejected { exit::REJECTED } else { exit::OK })
}

fn order_detail(o: &Order, enc: &PriceEncoding) -> String {
    crate::output::kv(&[
        ("order_id", o.order_id.clone()),
        ("client_order_id", o.client_order_id.clone()),
        ("ticker", o.ticker.clone()),
        ("side/action", format!("{} {}", o.action, o.side)),
        (
            "price",
            format!("{} ({} ticks)", enc.format(o.price_ticks), o.price_ticks),
        ),
        (
            "count",
            format!(
                "{} filled {} / remaining {} / cancelled {}",
                o.count, o.filled_count, o.remaining_count, o.cancelled_count
            ),
        ),
        (
            "avg fill",
            if o.avg_fill_price_ticks > 0 {
                enc.format(o.avg_fill_price_ticks)
            } else {
                "-".into()
            },
        ),
        ("status", o.status.clone()),
        (
            "trigger",
            o.trigger_price_ticks
                .map(|t| {
                    format!(
                        "{} at {} ({} ticks)",
                        o.order_type.clone().unwrap_or_default(),
                        enc.format(t),
                        t
                    )
                })
                .unwrap_or_default(),
        ),
        ("fired as", o.fired_order_id.clone().unwrap_or_default()),
        (
            "reason",
            o.reject_code
                .clone()
                .filter(|c| !c.is_empty())
                .unwrap_or_default(),
        ),
        (
            "tif",
            format!(
                "{}{}{}",
                o.tif,
                if o.post_only { " post-only" } else { "" },
                if o.reduce_only { " reduce-only" } else { "" }
            ),
        ),
        ("created", o.created_at.clone().unwrap_or_default()),
        ("updated", o.updated_at.clone().unwrap_or_default()),
    ])
}

async fn list(
    ctx: &Ctx,
    ticker: Option<String>,
    status: &str,
    limit: u32,
    cursor: Option<String>,
    all: bool,
) -> Result<i32> {
    let status = if status.eq_ignore_ascii_case("any") || status.eq_ignore_ascii_case("all") {
        None
    } else {
        Some(status.to_ascii_uppercase().replace('-', "_"))
    };
    let mut filter = OrderFilter {
        ticker: ticker.clone(),
        status,
        limit: Some(limit.clamp(1, 500)),
        cursor,
    };
    let mut items: Vec<Order> = Vec::new();
    let next = loop {
        let page = ctx.client().list_orders(&filter).await?;
        items.extend(page.items);
        if !all || page.next_cursor.is_empty() {
            break page.next_cursor;
        }
        filter.cursor = Some(page.next_cursor);
    };
    // Resolve encodings for the distinct tickers (bounded to keep this cheap).
    let mut encs: HashMap<String, PriceEncoding> = HashMap::new();
    for t in items
        .iter()
        .map(|o| o.ticker.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(40)
    {
        let e = ctx.encoding(&t).await;
        encs.insert(t, e);
    }
    ctx.out.emit(
        &json!({ "count": items.len(), "orders": items, "next_cursor": next }),
        || {
            let mut t = table(&[
                "order_id",
                "ticker",
                "side",
                "price",
                "filled/count",
                "status",
                "tif",
                "updated",
            ]);
            for o in &items {
                let fmt = |ticks: i64| {
                    encs.get(&o.ticker)
                        .map(|e| e.format(ticks))
                        .unwrap_or_else(|| format!("{ticks} ticks"))
                };
                let px = match o.trigger_price_ticks {
                    // A trigger order shows what arms it (and its limit price, if it has one).
                    Some(t) if o.price_ticks > 0 => {
                        format!("trigger {} → {}", fmt(t), fmt(o.price_ticks))
                    }
                    Some(t) => format!("trigger {}", fmt(t)),
                    None => fmt(o.price_ticks),
                };
                t.add_row(vec![
                    o.order_id.clone(),
                    o.ticker.clone(),
                    format!("{} {}", o.action, o.side),
                    px,
                    format!("{}/{}", o.filled_count, o.count),
                    o.status.clone(),
                    o.tif.clone(),
                    short_ts(&o.updated_at),
                ]);
            }
            let mut s = t.to_string();
            if !next.is_empty() {
                s.push_str(&format!("\nmore: --cursor {next}"));
            }
            s
        },
    );
    Ok(exit::OK)
}
