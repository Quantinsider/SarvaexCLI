//! WebSocket client for `gw-ws` plus a sequence-checked local order book.
//!
//! Rules from the exchange: apply a `market_book_delta` only when its
//! `book_seq` follows the last applied one; on a gap, discard the local book
//! and re-snapshot (`LocalBook::needs_resync`). `new_total_qty` is
//! authoritative, `qty_delta` is only a consistency check.

use crate::client::Credential;
use crate::error::{Result, SarvaexError};
use crate::models::{BookLevel, WsEvent};
use futures::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A live connection to the WebSocket gateway.
pub struct WsConnection {
    sink: SplitSink<WsStream, Message>,
    stream: SplitStream<WsStream>,
}

impl WsConnection {
    /// Open the socket. The credential (if any) is sent as an upgrade header so
    /// the `private` channel can be subscribed. API keys need `websocket:read`.
    pub async fn connect(ws_url: &Url, credential: &Credential) -> Result<Self> {
        let mut request = ws_url
            .as_str()
            .into_client_request()
            .map_err(|e| SarvaexError::WebSocket(format!("bad ws url: {e}")))?;
        match credential {
            Credential::ApiKey(k) => {
                request.headers_mut().insert(
                    "x-api-key",
                    k.parse()
                        .map_err(|_| SarvaexError::WebSocket("invalid api key header".into()))?,
                );
            }
            Credential::Bearer(t) => {
                request.headers_mut().insert(
                    "authorization",
                    format!("Bearer {t}")
                        .parse()
                        .map_err(|_| SarvaexError::WebSocket("invalid bearer header".into()))?,
                );
            }
            Credential::None => {}
        }
        let (ws, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| SarvaexError::WebSocket(format!("connect failed: {e}")))?;
        let (sink, stream) = ws.split();
        Ok(Self { sink, stream })
    }

    pub async fn subscribe_market(&mut self, ticker: &str) -> Result<()> {
        self.send_json(&json!({ "op": "subscribe", "channel": "market", "ticker": ticker }))
            .await
    }

    pub async fn subscribe_private(&mut self, ticker: &str) -> Result<()> {
        self.send_json(&json!({ "op": "subscribe", "channel": "private", "ticker": ticker }))
            .await
    }

    /// Subscribe to any gateway channel: `orders`, `lifecycle` and `exchange` take no
    /// ticker; `ticker` and `candles` need one (and `candles` an `interval`).
    pub async fn subscribe_channel(
        &mut self,
        channel: &str,
        ticker: Option<&str>,
        interval: Option<&str>,
    ) -> Result<()> {
        let mut msg = json!({ "op": "subscribe", "channel": channel });
        if let Some(t) = ticker {
            msg["ticker"] = json!(t);
        }
        if let Some(i) = interval {
            msg["interval"] = json!(i);
        }
        self.send_json(&msg).await
    }

    pub async fn send_json(&mut self, v: &Value) -> Result<()> {
        self.sink
            .send(Message::text(v.to_string()))
            .await
            .map_err(|e| SarvaexError::WebSocket(format!("send failed: {e}")))
    }

    /// Next decoded event. Returns `None` when the server closes the socket.
    /// Ping/pong/binary frames are handled internally.
    pub async fn next_event(&mut self) -> Option<Result<(WsEvent, Value)>> {
        loop {
            let msg = match self.stream.next().await? {
                Ok(m) => m,
                Err(e) => return Some(Err(SarvaexError::WebSocket(e.to_string()))),
            };
            match msg {
                Message::Text(text) => {
                    let raw: Value = match serde_json::from_str(text.as_str()) {
                        Ok(v) => v,
                        Err(e) => {
                            return Some(Err(SarvaexError::WebSocket(format!(
                                "invalid json frame: {e}"
                            ))));
                        }
                    };
                    let event: WsEvent =
                        serde_json::from_value(raw.clone()).unwrap_or(WsEvent::Other);
                    return Some(Ok((event, raw)));
                }
                Message::Ping(payload) => {
                    if let Err(e) = self.sink.send(Message::Pong(payload)).await {
                        return Some(Err(SarvaexError::WebSocket(e.to_string())));
                    }
                }
                Message::Close(_) => return None,
                _ => {}
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.sink.send(Message::Close(None)).await;
    }
}

/// Result of applying one event to a [`LocalBook`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookUpdate {
    /// Book changed.
    Applied,
    /// Event was for another ticker or not a book event.
    Ignored,
    /// Delta did not follow the last `book_seq`; caller must re-snapshot.
    Gap { expected: i64, got: i64 },
}

/// Order book maintained from WebSocket snapshots and deltas.
#[derive(Debug, Clone, Default)]
pub struct LocalBook {
    pub ticker: String,
    pub book_seq: i64,
    pub synced: bool,
    bids: BTreeMap<i64, i64>,
    asks: BTreeMap<i64, i64>,
}

impl LocalBook {
    pub fn new(ticker: impl Into<String>) -> Self {
        Self {
            ticker: ticker.into(),
            ..Default::default()
        }
    }

    /// Seed from a REST snapshot (`seq` is the book sequence).
    pub fn load_snapshot(&mut self, seq: i64, bids: &[BookLevel], asks: &[BookLevel]) {
        self.bids = bids
            .iter()
            .filter(|l| l.total_qty > 0)
            .map(|l| (l.price_ticks, l.total_qty))
            .collect();
        self.asks = asks
            .iter()
            .filter(|l| l.total_qty > 0)
            .map(|l| (l.price_ticks, l.total_qty))
            .collect();
        self.book_seq = seq;
        self.synced = true;
    }

    pub fn needs_resync(&self) -> bool {
        !self.synced
    }

    pub fn apply(&mut self, event: &WsEvent) -> BookUpdate {
        match event {
            WsEvent::MarketBookSnapshot {
                ticker,
                book_seq,
                seq,
                bids,
                asks,
            } if *ticker == self.ticker => {
                let s = if *book_seq > 0 { *book_seq } else { *seq };
                self.load_snapshot(s, bids, asks);
                BookUpdate::Applied
            }
            WsEvent::MarketBookDelta {
                ticker,
                book_seq,
                book_side,
                price_ticks,
                new_total_qty,
                ..
            } if *ticker == self.ticker => {
                if !self.synced {
                    return BookUpdate::Gap {
                        expected: self.book_seq + 1,
                        got: *book_seq,
                    };
                }
                if *book_seq != self.book_seq + 1 {
                    if *book_seq <= self.book_seq {
                        // Stale/duplicate delta: safe to ignore.
                        return BookUpdate::Ignored;
                    }
                    self.synced = false;
                    return BookUpdate::Gap {
                        expected: self.book_seq + 1,
                        got: *book_seq,
                    };
                }
                let side = if book_side.eq_ignore_ascii_case("BID") {
                    &mut self.bids
                } else {
                    &mut self.asks
                };
                if *new_total_qty <= 0 {
                    side.remove(price_ticks);
                } else {
                    side.insert(*price_ticks, *new_total_qty);
                }
                self.book_seq = *book_seq;
                BookUpdate::Applied
            }
            _ => BookUpdate::Ignored,
        }
    }

    /// Bids best-first (descending price).
    pub fn bids(&self) -> Vec<BookLevel> {
        self.bids
            .iter()
            .rev()
            .map(|(p, q)| BookLevel {
                price_ticks: *p,
                total_qty: *q,
                order_count: 0,
            })
            .collect()
    }
    /// Asks best-first (ascending price).
    pub fn asks(&self) -> Vec<BookLevel> {
        self.asks
            .iter()
            .map(|(p, q)| BookLevel {
                price_ticks: *p,
                total_qty: *q,
                order_count: 0,
            })
            .collect()
    }
    pub fn best_bid(&self) -> Option<(i64, i64)> {
        self.bids.iter().next_back().map(|(p, q)| (*p, *q))
    }
    pub fn best_ask(&self) -> Option<(i64, i64)> {
        self.asks.iter().next().map(|(p, q)| (*p, *q))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(seq: i64, side: &str, px: i64, qty: i64) -> WsEvent {
        WsEvent::MarketBookDelta {
            event_id: String::new(),
            ticker: "T".into(),
            global_seq: seq,
            contract_seq: seq,
            book_seq: seq,
            book_side: side.into(),
            side: Value::Null,
            price_ticks: px,
            qty_delta: 0,
            new_total_qty: qty,
        }
    }

    #[test]
    fn applies_in_sequence_and_detects_gaps() {
        let mut book = LocalBook::new("T");
        assert!(book.needs_resync());
        book.load_snapshot(
            10,
            &[BookLevel {
                price_ticks: 49,
                total_qty: 100,
                order_count: 1,
            }],
            &[BookLevel {
                price_ticks: 51,
                total_qty: 50,
                order_count: 1,
            }],
        );
        assert_eq!(book.apply(&delta(11, "BID", 49, 90)), BookUpdate::Applied);
        assert_eq!(book.best_bid(), Some((49, 90)));
        assert_eq!(
            book.apply(&delta(11, "BID", 49, 1)),
            BookUpdate::Ignored,
            "duplicate ignored"
        );
        assert_eq!(book.apply(&delta(12, "ASK", 51, 0)), BookUpdate::Applied);
        assert_eq!(book.best_ask(), None);
        assert_eq!(
            book.apply(&delta(14, "BID", 48, 5)),
            BookUpdate::Gap {
                expected: 13,
                got: 14
            }
        );
        assert!(book.needs_resync());
    }
}
