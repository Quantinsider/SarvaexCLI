use super::Ctx;
use crate::cli::RfqCmd;
use crate::output::{exit, kv, short_ts, table};
use anyhow::Result;
use sarvaex_sdk::price::PriceEncoding;
use sarvaex_sdk::{NewQuote, NewRfq, Rfq};
use serde_json::json;
use uuid_like::simple_id;

mod uuid_like {
    pub fn simple_id(prefix: &str) -> String {
        format!(
            "{prefix}-{}",
            sarvaex_sdk::idempotency::client_order_id().trim_start_matches("cli-")
        )
    }
}

fn rfq_kv(r: &Rfq) -> String {
    kv(&[
        ("rfq_id", r.rfq_id.clone()),
        ("ticker", r.ticker.clone()),
        ("side/action", format!("{} {}", r.action, r.side)),
        ("requested", r.requested_count.to_string()),
        ("status", r.status.clone()),
        ("accepted quote", r.accepted_quote_id.clone()),
        ("expires", r.expires_at.clone().unwrap_or_default()),
    ])
}

pub async fn run(ctx: &Ctx, cmd: RfqCmd) -> Result<i32> {
    match cmd {
        RfqCmd::Create {
            ticker,
            side,
            action,
            count,
            expires_at,
            client_rfq_id,
        } => {
            let c = ctx.contract(&ticker).await?;
            let rfq = NewRfq {
                client_rfq_id: client_rfq_id.unwrap_or_else(|| simple_id("rfq")),
                ticker: c.ticker,
                side: side.into(),
                action: action.into(),
                requested_count: count,
                expires_at,
            };
            let r = ctx.client().create_rfq(&rfq, None).await?;
            ctx.out.emit(&r, || rfq_kv(&r));
        }
        RfqCmd::Get { rfq_id } => {
            let r = ctx.client().get_rfq(&rfq_id).await?;
            ctx.out.emit(&r, || rfq_kv(&r));
        }
        RfqCmd::Quotes { rfq_id, limit } => {
            let r = ctx.client().get_rfq(&rfq_id).await?;
            let enc = ctx.encoding(&r.ticker).await;
            let page = ctx
                .client()
                .list_rfq_quotes(&rfq_id, Some(limit), None)
                .await?;
            ctx.out.emit(
                &json!({ "rfq": r, "quotes": page.items, "next_cursor": page.next_cursor }),
                || {
                    let mut t = table(&["quote_id", "bid", "offer", "count", "status", "expires"]);
                    for q in &page.items {
                        t.add_row(vec![
                            q.quote_id.clone(),
                            enc.format(q.bid_price_ticks),
                            enc.format(q.offer_price_ticks),
                            q.available_count.to_string(),
                            q.status.clone(),
                            short_ts(&q.expires_at),
                        ]);
                    }
                    t.to_string()
                },
            );
        }
        RfqCmd::Quote {
            rfq_id,
            bid,
            bid_ticks,
            offer,
            offer_ticks,
            count,
            expires_at,
            quote_id,
        } => {
            let r = ctx.client().get_rfq(&rfq_id).await?;
            let c = ctx.contract(&r.ticker).await?;
            let enc = PriceEncoding::for_contract(&c);
            let bid_t = super::orders::resolve_price(&enc, bid.as_deref(), bid_ticks)?;
            let offer_t = super::orders::resolve_price(&enc, offer.as_deref(), offer_ticks)?;
            let q = NewQuote {
                quote_id: quote_id.unwrap_or_else(|| simple_id("quote")),
                bid_price_ticks: bid_t,
                offer_price_ticks: offer_t,
                available_count: count,
                expires_at,
            };
            let res = ctx.client().submit_rfq_quote(&rfq_id, &q, None).await?;
            ctx.out.emit(&res, || {
                format!(
                    "quote {} submitted: {} / {} × {} ({})",
                    res.quote_id,
                    enc.format(res.bid_price_ticks),
                    enc.format(res.offer_price_ticks),
                    res.available_count,
                    res.status
                )
            });
        }
        RfqCmd::Accept { rfq_id, quote_id } => {
            if !ctx.confirm(&format!("Accept quote {quote_id} on RFQ {rfq_id}?")) {
                anyhow::bail!("aborted");
            }
            let v = ctx
                .client()
                .accept_rfq_quote(&rfq_id, &quote_id, None)
                .await?;
            ctx.out
                .emit(&v, || serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        RfqCmd::Cancel { rfq_id } => {
            let r = ctx.client().cancel_rfq(&rfq_id, None).await?;
            ctx.out.emit(&r, || rfq_kv(&r));
        }
        RfqCmd::CancelQuote { rfq_id, quote_id } => {
            let q = ctx
                .client()
                .cancel_rfq_quote(&rfq_id, &quote_id, None)
                .await?;
            ctx.out
                .emit(&q, || format!("quote {} is now {}", q.quote_id, q.status));
        }
    }
    Ok(exit::OK)
}
