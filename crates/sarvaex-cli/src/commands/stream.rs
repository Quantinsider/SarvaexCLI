use super::Ctx;
use crate::cli::StreamArgs;
use crate::output::exit;
use anyhow::{Context, Result};
use sarvaex_sdk::WsEvent;
use sarvaex_sdk::ws::{BookUpdate, LocalBook, WsConnection};
use serde_json::json;
use std::collections::HashMap;

/// Emits one JSON object per line. Book deltas are applied to a local book when
/// `--book` is set; on a sequence gap the book is re-fetched over REST.
pub async fn run(ctx: &Ctx, args: StreamArgs) -> Result<i32> {
    let tickers: Vec<String> = args
        .tickers
        .iter()
        .map(|t| t.trim().to_ascii_uppercase())
        .collect();
    let mut encs = HashMap::new();
    for t in &tickers {
        encs.insert(t.clone(), ctx.encoding(t).await);
    }
    let mut conn = WsConnection::connect(ctx.client().ws_url(), ctx.client().credential())
        .await
        .context("websocket connect")?;
    for t in &tickers {
        conn.subscribe_market(t).await?;
        if args.private {
            conn.subscribe_private(t).await?;
        }
    }
    let mut books: HashMap<String, LocalBook> = tickers
        .iter()
        .map(|t| (t.clone(), LocalBook::new(t.clone())))
        .collect();
    if args.book {
        for t in &tickers {
            resync(ctx, books.get_mut(t).unwrap()).await;
        }
    }
    let mut count = 0u64;
    loop {
        let next = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            n = conn.next_event() => n,
        };
        let Some(item) = next else { break };
        let (event, raw) = item?;
        count += 1;
        match &event {
            WsEvent::MarketBookSnapshot { ticker, .. }
            | WsEvent::MarketBookDelta { ticker, .. }
                if args.book =>
            {
                if let Some(book) = books.get_mut(ticker) {
                    match book.apply(&event) {
                        BookUpdate::Gap { expected, got } => {
                            eprintln!(
                                "{{\"type\":\"gap\",\"ticker\":\"{ticker}\",\"expected\":{expected},\"got\":{got},\"action\":\"resync\"}}"
                            );
                            resync(ctx, book).await;
                        }
                        BookUpdate::Ignored => continue,
                        BookUpdate::Applied => {}
                    }
                    let enc = encs.get(ticker).cloned().unwrap_or_default_enc();
                    let v = json!({
                        "type": "book",
                        "ticker": ticker,
                        "book_seq": book.book_seq,
                        "best_bid": book.best_bid().map(|(p, q)| json!({"price_ticks": p, "price": enc.format(p), "qty": q})),
                        "best_ask": book.best_ask().map(|(p, q)| json!({"price_ticks": p, "price": enc.format(p), "qty": q})),
                        "bids": book.bids().iter().take(10).map(|l| json!({"price_ticks": l.price_ticks, "price": enc.format(l.price_ticks), "qty": l.total_qty})).collect::<Vec<_>>(),
                        "asks": book.asks().iter().take(10).map(|l| json!({"price_ticks": l.price_ticks, "price": enc.format(l.price_ticks), "qty": l.total_qty})).collect::<Vec<_>>(),
                    });
                    println!("{v}");
                }
            }
            WsEvent::MarketTrade {
                ticker,
                price_ticks,
                ..
            }
            | WsEvent::PrivateFill {
                ticker,
                price_ticks,
                ..
            } => {
                let mut v = raw.clone();
                if let Some(enc) = encs.get(ticker) {
                    v["price"] = json!(enc.format(*price_ticks));
                }
                println!("{}", serde_json::to_string(&v)?);
            }
            WsEvent::Error { code, message } => {
                eprintln!(
                    "{}",
                    json!({"type": "error", "code": code, "message": message})
                );
                if code == "UNAUTHENTICATED" {
                    return Ok(exit::AUTH);
                }
            }
            _ => println!("{}", serde_json::to_string(&raw)?),
        }
        if args.max_events > 0 && count >= args.max_events {
            break;
        }
    }
    conn.close().await;
    Ok(exit::OK)
}

async fn resync(ctx: &Ctx, book: &mut LocalBook) {
    match ctx.client().orderbook(&book.ticker, Some(100)).await {
        Ok(snap) => book.load_snapshot(snap.seq, &snap.bids, &snap.asks),
        Err(e) => eprintln!(
            "{}",
            json!({"type": "error", "code": "RESYNC_FAILED", "ticker": book.ticker, "message": e.to_string()})
        ),
    }
}

trait OrDefaultEnc {
    fn unwrap_or_default_enc(self) -> sarvaex_sdk::PriceEncoding;
}
impl OrDefaultEnc for Option<sarvaex_sdk::PriceEncoding> {
    fn unwrap_or_default_enc(self) -> sarvaex_sdk::PriceEncoding {
        self.unwrap_or_else(sarvaex_sdk::PriceEncoding::binary)
    }
}
