use super::Ctx;
use crate::cli::{EventsCmd, MarketListArgs, MarketsCmd, SeriesCmd};
use crate::output::{exit, kv, short_ts, table, truncate};
use anyhow::{Result, anyhow};
use sarvaex_sdk::price::PriceEncoding;
use sarvaex_sdk::{Contract, ContractKind, ContractState, MarketFilter};
use serde_json::json;

pub fn parse_state(s: &str) -> Result<Option<ContractState>> {
    if s.eq_ignore_ascii_case("any") || s.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    s.parse::<ContractState>().map(Some).map_err(|e| anyhow!(e))
}

pub fn contract_row(c: &Contract) -> Vec<String> {
    let enc = PriceEncoding::for_contract(c);
    vec![
        c.ticker.clone(),
        c.kind().as_str().to_owned(),
        c.state().as_str().to_owned(),
        c.category().unwrap_or("").to_owned(),
        truncate(&c.question, 70),
        format!(
            "{}–{}",
            enc.format(c.min_price_ticks.max(1)),
            enc.format(c.max_price_ticks)
        ),
        short_ts(&c.close_at),
    ]
}

pub fn contracts_table(items: &[Contract]) -> String {
    let mut t = table(&[
        "ticker",
        "kind",
        "state",
        "category",
        "question",
        "price range",
        "closes",
    ]);
    for c in items {
        t.add_row(contract_row(c));
    }
    t.to_string()
}

pub async fn markets(ctx: &Ctx, cmd: MarketsCmd) -> Result<i32> {
    match cmd {
        MarketsCmd::List(args) => list(ctx, args).await,
        MarketsCmd::Search {
            query,
            state,
            limit,
        } => search(ctx, query, &state, limit).await,
        MarketsCmd::Get { ticker } => {
            let c = ctx.contract(&ticker).await?;
            let enc = PriceEncoding::for_contract(&c);
            let mut v = serde_json::to_value(&c)?;
            v["kind_name"] = json!(c.kind().as_str());
            v["state_name"] = json!(c.state().as_str());
            v["price_encoding"] = json!(enc);
            ctx.out.emit(&v, || {
                kv(&[
                    ("ticker", c.ticker.clone()),
                    (
                        "kind",
                        format!(
                            "{} ({})",
                            c.kind().as_str(),
                            if c.kind() == ContractKind::Binary {
                                "YES/NO"
                            } else {
                                "LONG/SHORT"
                            }
                        ),
                    ),
                    ("state", c.state().as_str().to_owned()),
                    ("question", c.question.clone()),
                    ("category", c.category().unwrap_or("").to_owned()),
                    ("underlying", c.underlying.clone()),
                    ("event", c.event_ticker.clone()),
                    ("series", c.series_ticker.clone()),
                    (
                        "price",
                        format!(
                            "{}–{} (1 tick = {}, tick_size {})",
                            enc.format(c.min_price_ticks.max(1)),
                            enc.format(c.max_price_ticks),
                            enc.format(1),
                            c.tick_size
                        ),
                    ),
                    ("max order", c.max_order_size.to_string()),
                    ("position limit", c.position_limit_per_user.to_string()),
                    ("opens", c.open_at.clone().unwrap_or_default()),
                    ("closes", c.close_at.clone().unwrap_or_default()),
                    (
                        "resolves",
                        c.expected_resolution_at.clone().unwrap_or_default(),
                    ),
                    ("settlement", c.settlement_source.clone()),
                    ("oracle", c.oracle_policy.clone()),
                ])
            });
            Ok(exit::OK)
        }
        MarketsCmd::Book { ticker, depth } => {
            let c = ctx.contract(&ticker).await?;
            let enc = PriceEncoding::for_contract(&c);
            let book = ctx.client().orderbook(&c.ticker, Some(depth)).await?;
            let mut bids = book.bids.clone();
            bids.sort_by_key(|l| std::cmp::Reverse(l.price_ticks));
            let mut asks = book.asks.clone();
            asks.sort_by_key(|l| l.price_ticks);
            let v = json!({
                "ticker": book.ticker, "seq": book.seq, "ts": book.ts,
                "best_bid": book.best_bid().map(|l| json!({"price_ticks": l.price_ticks, "price": enc.format(l.price_ticks), "qty": l.total_qty})),
                "best_ask": book.best_ask().map(|l| json!({"price_ticks": l.price_ticks, "price": enc.format(l.price_ticks), "qty": l.total_qty})),
                "bids": bids.iter().map(|l| json!({"price_ticks": l.price_ticks, "price": enc.format(l.price_ticks), "qty": l.total_qty, "orders": l.order_count})).collect::<Vec<_>>(),
                "asks": asks.iter().map(|l| json!({"price_ticks": l.price_ticks, "price": enc.format(l.price_ticks), "qty": l.total_qty, "orders": l.order_count})).collect::<Vec<_>>(),
            });
            ctx.out.emit(&v, || {
                let mut t = table(&["bid qty", "bid", "ask", "ask qty"]);
                let n = bids.len().max(asks.len());
                for i in 0..n {
                    let b = bids.get(i);
                    let a = asks.get(i);
                    t.add_row(vec![
                        b.map(|l| l.total_qty.to_string()).unwrap_or_default(),
                        b.map(|l| enc.format(l.price_ticks)).unwrap_or_default(),
                        a.map(|l| enc.format(l.price_ticks)).unwrap_or_default(),
                        a.map(|l| l.total_qty.to_string()).unwrap_or_default(),
                    ]);
                }
                let mut head = format!("{}  seq {}", book.ticker, book.seq);
                if c.kind() == ContractKind::Binary
                    && let Some(a) = book.best_ask()
                {
                    head.push_str(&format!(
                        "  implied YES ≈ {}%  (NO ask = {})",
                        a.price_ticks,
                        enc.format(100 - a.price_ticks)
                    ));
                }
                format!("{head}\n{t}")
            });
            Ok(exit::OK)
        }
        MarketsCmd::Fills {
            ticker,
            limit,
            cursor,
        } => {
            let c = ctx.contract(&ticker).await?;
            let enc = PriceEncoding::for_contract(&c);
            let page = ctx
                .client()
                .market_fills(&c.ticker, None, None, Some(limit), cursor.as_deref())
                .await?;
            ctx.out.emit(
                &json!({ "fills": page.items, "next_cursor": page.next_cursor }),
                || {
                    let mut t = table(&["time", "price", "count", "aggressor", "global_seq"]);
                    for f in &page.items {
                        t.add_row(vec![
                            short_ts(&f.ts),
                            enc.format(f.price_ticks),
                            f.count.to_string(),
                            f.aggressor_side.to_string().replace('"', ""),
                            f.global_seq.to_string(),
                        ]);
                    }
                    t.to_string()
                },
            );
            Ok(exit::OK)
        }
        MarketsCmd::Oi { ticker } => {
            let oi = ctx.client().open_interest(&ticker).await?;
            ctx.out.emit(&oi, || {
                kv(&[
                    ("ticker", oi.ticker.clone()),
                    ("open long", oi.total_open_long.to_string()),
                    ("open short", oi.total_open_short.to_string()),
                    ("as of", oi.as_of.clone().unwrap_or_default()),
                ])
            });
            Ok(exit::OK)
        }
        MarketsCmd::Settlement { ticker } => {
            let v = ctx.client().get_settlement(&ticker).await?;
            ctx.out
                .emit(&v, || serde_json::to_string_pretty(&v).unwrap_or_default());
            Ok(exit::OK)
        }
    }
}

async fn list(ctx: &Ctx, args: MarketListArgs) -> Result<i32> {
    let filter = MarketFilter {
        state: parse_state(&args.state)?,
        kind: match args.kind.as_deref() {
            Some(k) => Some(k.parse::<ContractKind>().map_err(|e| anyhow!(e))?),
            None => None,
        },
        series_ticker: args.series,
        event_ticker: args.event,
        underlying: args.underlying,
        category: args.category,
        limit: Some(args.limit.clamp(1, 500)),
        cursor: args.cursor,
        ..Default::default()
    };
    let (items, next) = if args.all {
        (
            ctx.client().list_all_markets(&filter, 50).await?,
            String::new(),
        )
    } else {
        let page = ctx.client().list_markets(&filter).await?;
        (page.items, page.next_cursor)
    };
    ctx.out.emit(
        &json!({ "count": items.len(), "contracts": items, "next_cursor": next }),
        || {
            let mut s = contracts_table(&items);
            if !next.is_empty() {
                s.push_str(&format!("\nmore: --cursor {next}"));
            }
            s
        },
    );
    Ok(exit::OK)
}

async fn search(ctx: &Ctx, query: Vec<String>, state: &str, limit: usize) -> Result<i32> {
    let words: Vec<String> = query
        .iter()
        .flat_map(|q| q.split_whitespace())
        .map(|w| w.to_lowercase())
        .collect();
    let all = ctx
        .client()
        .list_all_markets(
            &MarketFilter {
                state: parse_state(state)?,
                ..Default::default()
            },
            20,
        )
        .await?;
    let hits: Vec<Contract> = all
        .into_iter()
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
        .take(limit.max(1))
        .collect();
    ctx.out.emit(
        &json!({ "query": words.join(" "), "count": hits.len(), "contracts": hits }),
        || {
            if hits.is_empty() {
                "no matching markets".to_owned()
            } else {
                contracts_table(&hits)
            }
        },
    );
    Ok(exit::OK)
}

pub async fn events(ctx: &Ctx, cmd: EventsCmd) -> Result<i32> {
    match cmd {
        EventsCmd::List {
            series,
            limit,
            cursor,
        } => {
            let page = ctx
                .client()
                .list_events(
                    series.as_deref(),
                    None,
                    None,
                    Some(limit),
                    cursor.as_deref(),
                )
                .await?;
            ctx.out.emit(
                &json!({ "events": page.items, "next_cursor": page.next_cursor }),
                || {
                    let mut t = table(&["event", "series", "title", "resolves"]);
                    for e in &page.items {
                        t.add_row(vec![
                            e.event_ticker.clone(),
                            e.series_ticker.clone(),
                            truncate(&e.title, 60),
                            short_ts(&e.expected_resolution_at),
                        ]);
                    }
                    t.to_string()
                },
            );
        }
        EventsCmd::Get { event_ticker } => {
            let e = ctx.client().get_event(&event_ticker).await?;
            ctx.out
                .emit(&e, || serde_json::to_string_pretty(&e).unwrap_or_default());
        }
        EventsCmd::Resolution { event_ticker } => {
            let v = ctx.client().get_resolution(&event_ticker).await?;
            ctx.out
                .emit(&v, || serde_json::to_string_pretty(&v).unwrap_or_default());
        }
    }
    Ok(exit::OK)
}

pub async fn series(ctx: &Ctx, cmd: SeriesCmd) -> Result<i32> {
    match cmd {
        SeriesCmd::List { limit, cursor } => {
            let page = ctx
                .client()
                .list_series(Some(limit), cursor.as_deref())
                .await?;
            ctx.out.emit(
                &json!({ "series": page.items, "next_cursor": page.next_cursor }),
                || {
                    let mut t = table(&["series", "title", "description"]);
                    for s in &page.items {
                        t.add_row(vec![
                            s.series_ticker.clone(),
                            s.title.clone(),
                            truncate(&s.description, 60),
                        ]);
                    }
                    t.to_string()
                },
            );
        }
    }
    Ok(exit::OK)
}
