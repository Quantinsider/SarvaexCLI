use super::Ctx;
use crate::cli::{AccountCmd, DemoCmd, MarginCmd, PositionsCmd};
use crate::output::{exit, kv, short_ts, table};
use anyhow::Result;
use sarvaex_sdk::FillFilter;
use sarvaex_sdk::price::{self, PriceEncoding};
use serde_json::json;

pub async fn account(ctx: &Ctx, cmd: AccountCmd) -> Result<i32> {
    match cmd {
        AccountCmd::Balance => {
            let b = ctx.client().balance().await?;
            let mut v = serde_json::to_value(&b)?;
            v["display"] = json!({ "cash_usdc": price::micro_to_usdc(b.cash_micro_usdc), "held_usdc": price::micro_to_usdc(b.held_micro_usdc), "total_usdc": price::micro_to_usdc(b.total_micro_usdc) });
            ctx.out.emit(&v, || {
                kv(&[
                    ("user", b.user_id.clone()),
                    (
                        "cash",
                        format!("{} USDC", price::micro_to_usdc(b.cash_micro_usdc)),
                    ),
                    (
                        "held",
                        format!("{} USDC", price::micro_to_usdc(b.held_micro_usdc)),
                    ),
                    (
                        "total",
                        format!("{} USDC", price::micro_to_usdc(b.total_micro_usdc)),
                    ),
                ])
            });
        }
        AccountCmd::Risk => {
            let r = ctx.client().account_risk().await?;
            let mut v = serde_json::to_value(&r)?;
            v["display"] = json!({
                "available_collateral_usdc": price::micro_to_usdc(r.available_collateral_micro_usdc),
                "equity_usdc": price::micro_to_usdc(r.equity_micro_usdc),
                "position_cost_usdc": price::micro_to_usdc(r.position_cost_micro_usdc),
                "realized_pnl_usdc": price::micro_to_usdc(r.realized_pnl_micro_usdc),
                "unrealized_pnl_usdc": price::micro_to_usdc(r.unrealized_pnl_micro_usdc),
            });
            ctx.out.emit(&v, || {
                let mut rows = vec![
                    ("user", r.user_id.clone()),
                    (
                        "available collateral",
                        format!(
                            "{} USDC",
                            price::micro_to_usdc(r.available_collateral_micro_usdc)
                        ),
                    ),
                    (
                        "equity",
                        format!("{} USDC", price::micro_to_usdc(r.equity_micro_usdc)),
                    ),
                    (
                        "position cost",
                        format!("{} USDC", price::micro_to_usdc(r.position_cost_micro_usdc)),
                    ),
                    (
                        "realized P&L",
                        format!("{} USDC", price::micro_to_usdc(r.realized_pnl_micro_usdc)),
                    ),
                    (
                        "unrealized P&L",
                        format!("{} USDC", price::micro_to_usdc(r.unrealized_pnl_micro_usdc)),
                    ),
                    ("open positions", r.open_positions.to_string()),
                    ("open orders", r.open_orders.to_string()),
                ];
                if let Some(l) = &r.limits {
                    rows.push(("kyc tier", l.kyc_tier.to_string()));
                    rows.push((
                        "max order size",
                        format!("{} USDC", price::micro_to_usdc(l.max_order_size_micro_usdc)),
                    ));
                    rows.push((
                        "daily loss limit",
                        format!(
                            "{} USDC",
                            price::micro_to_usdc(l.daily_loss_limit_micro_usdc)
                        ),
                    ));
                    rows.push((
                        "per-contract limits",
                        l.per_contract_position_limit.to_string(),
                    ));
                }
                kv(&rows)
            });
        }
        AccountCmd::Margin => {
            let v = ctx.client().account_margin().await?;
            ctx.out
                .emit(&v, || serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        AccountCmd::Profile => {
            let p = ctx.client().profile().await?;
            ctx.out.emit(&p, || {
                kv(&[
                    ("user_id", p.user_id.clone()),
                    ("name", p.name.clone()),
                    ("email", p.email.clone()),
                    ("status", p.status.clone()),
                ])
            });
        }
        AccountCmd::Fills {
            ticker,
            order_id,
            limit,
            cursor,
        } => {
            let page = ctx
                .client()
                .account_fills(&FillFilter {
                    ticker,
                    order_id,
                    limit: Some(limit.clamp(1, 500)),
                    cursor,
                    ..Default::default()
                })
                .await?;
            let mut encs = std::collections::HashMap::new();
            for t in page
                .items
                .iter()
                .map(|f| f.ticker.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .take(40)
            {
                let e = ctx.encoding(&t).await;
                encs.insert(t, e);
            }
            ctx.out.emit(
                &json!({ "fills": page.items, "next_cursor": page.next_cursor }),
                || {
                    let mut t = table(&[
                        "time", "ticker", "role", "side", "price", "count", "fee", "order_id",
                    ]);
                    for f in &page.items {
                        let px = encs
                            .get(&f.ticker)
                            .map(|e| e.format(f.price_ticks))
                            .unwrap_or_else(|| f.price_ticks.to_string());
                        t.add_row(vec![
                            short_ts(&f.ts),
                            f.ticker.clone(),
                            f.role.clone(),
                            format!("{} {}", f.action, f.side),
                            px,
                            f.count.to_string(),
                            price::micro_to_usdc(f.fee_micro_usdc),
                            f.order_id.clone(),
                        ]);
                    }
                    let mut s = t.to_string();
                    if !page.next_cursor.is_empty() {
                        s.push_str(&format!("\nmore: --cursor {}", page.next_cursor));
                    }
                    s
                },
            );
        }
        AccountCmd::History { limit, cursor } => {
            let page = ctx
                .client()
                .account_history(Some(limit.clamp(1, 500)), cursor.as_deref())
                .await?;
            ctx.out.emit(
                &json!({ "entries": page.items, "next_cursor": page.next_cursor }),
                || {
                    let mut t = table(&["posted", "dir", "amount", "running", "reason", "memo"]);
                    for e in &page.items {
                        t.add_row(vec![
                            short_ts(&e.posted_at),
                            e.direction.clone(),
                            price::micro_to_usdc(e.amount_micro_usdc),
                            price::micro_to_usdc(e.running_balance_micro_usdc),
                            e.reason_code.clone(),
                            e.memo.clone(),
                        ]);
                    }
                    t.to_string()
                },
            );
        }
    }
    Ok(exit::OK)
}

pub async fn positions(ctx: &Ctx, cmd: PositionsCmd) -> Result<i32> {
    match cmd {
        PositionsCmd::List {
            include_closed,
            limit,
            cursor,
        } => {
            let page = ctx
                .client()
                .positions(include_closed, Some(limit.clamp(1, 500)), cursor.as_deref())
                .await?;
            ctx.out.emit(
                &json!({ "positions": page.items, "next_cursor": page.next_cursor }),
                || {
                    let mut t = table(&[
                        "ticker",
                        "net qty",
                        "avg cost",
                        "realized P&L",
                        "unrealized P&L",
                        "updated",
                    ]);
                    for p in &page.items {
                        t.add_row(vec![
                            p.ticker.clone(),
                            p.net_qty.to_string(),
                            price::micro_to_usdc(p.avg_cost_micro_usdc),
                            price::micro_to_usdc(p.realized_pnl_micro_usdc),
                            price::micro_to_usdc(p.unrealized_pnl_micro_usdc),
                            short_ts(&p.updated_at),
                        ]);
                    }
                    if page.items.is_empty() {
                        "no open positions".to_owned()
                    } else {
                        t.to_string()
                    }
                },
            );
        }
        PositionsCmd::Get { ticker } => {
            let p = ctx.client().position(&ticker).await?;
            ctx.out.emit(&p, || {
                kv(&[
                    ("ticker", p.ticker.clone()),
                    ("net qty", p.net_qty.to_string()),
                    (
                        "avg cost",
                        format!("{} USDC", price::micro_to_usdc(p.avg_cost_micro_usdc)),
                    ),
                    (
                        "realized P&L",
                        format!("{} USDC", price::micro_to_usdc(p.realized_pnl_micro_usdc)),
                    ),
                    (
                        "unrealized P&L",
                        format!("{} USDC", price::micro_to_usdc(p.unrealized_pnl_micro_usdc)),
                    ),
                    ("updated", p.updated_at.clone().unwrap_or_default()),
                ])
            });
        }
    }
    Ok(exit::OK)
}

pub async fn margin(ctx: &Ctx, cmd: MarginCmd) -> Result<i32> {
    match cmd {
        MarginCmd::Preview {
            ticker,
            side,
            action,
            count,
            price,
            price_ticks,
        } => {
            let c = ctx.contract(&ticker).await?;
            let enc = PriceEncoding::for_contract(&c);
            let ticks = super::orders::resolve_price(&enc, price.as_deref(), price_ticks)?;
            let m = ctx
                .client()
                .preview_margin(&c.ticker, side.into(), action.into(), ticks, count)
                .await?;
            let mut v = serde_json::to_value(&m)?;
            v["display"] = json!({ "margin_before_usdc": price::micro_to_usdc(m.margin_before_micro_usdc), "margin_after_usdc": price::micro_to_usdc(m.margin_after_micro_usdc), "delta_usdc": price::micro_to_usdc(m.delta_micro_usdc) });
            ctx.out.emit(&v, || {
                kv(&[
                    ("event", m.event_ticker.clone()),
                    (
                        "eligible",
                        format!(
                            "{}{}",
                            m.eligible,
                            if m.ineligible_reason.is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", m.ineligible_reason)
                            }
                        ),
                    ),
                    (
                        "margin before",
                        format!("{} USDC", price::micro_to_usdc(m.margin_before_micro_usdc)),
                    ),
                    (
                        "margin after",
                        format!("{} USDC", price::micro_to_usdc(m.margin_after_micro_usdc)),
                    ),
                    (
                        "delta",
                        format!("{} USDC", price::micro_to_usdc(m.delta_micro_usdc)),
                    ),
                    (
                        "standalone delta",
                        format!(
                            "{} USDC",
                            price::micro_to_usdc(m.standalone_delta_micro_usdc)
                        ),
                    ),
                ])
            });
        }
    }
    Ok(exit::OK)
}

pub async fn demo(ctx: &Ctx, cmd: DemoCmd) -> Result<i32> {
    match cmd {
        DemoCmd::Credit { usdc, note } => {
            let micro = price::usdc_to_micro(&usdc)?;
            let r = ctx
                .client()
                .demo_credit(micro, note.as_deref(), None)
                .await?;
            ctx.out.emit(&r, || {
                format!(
                    "credited {} USDC; balance now {} USDC",
                    price::micro_to_usdc(micro),
                    price::micro_to_usdc(r.balance.total_micro_usdc)
                )
            });
        }
    }
    Ok(exit::OK)
}
