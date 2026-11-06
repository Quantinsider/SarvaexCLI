//! # sarvaex-sdk
//!
//! Typed async client for the SarvaEX prediction-market exchange.
//!
//! ```no_run
//! use sarvaex_sdk::{Client, Credential, MarketFilter, NewOrder, Side, Action};
//!
//! # async fn demo() -> sarvaex_sdk::Result<()> {
//! let client = Client::with_credential("https://api.sarvaex.com", Credential::ApiKey("svx_live_...".into()))?;
//! let open = client.list_markets(&MarketFilter::open()).await?;
//! let order = NewOrder::limit("SX-FEDDEC-26OCT-H25", Side::Yes, Action::Buy, 51, 10);
//! let resp = client.place_order(&order, None).await?;
//! println!("{}", resp.summary()); // e.g. "FILLED 10/10 @ avg 51 ticks"
//! # Ok(()) }
//! ```
//!
//! Everything the exchange returns is an integer: `price_ticks`, `count`,
//! micro-USDC. Use [`price::PriceEncoding`] to convert to and from the human
//! prices a given contract displays.

pub mod client;
pub mod error;
pub mod idempotency;
pub mod models;
pub mod price;
pub mod ws;

pub use client::{
    Client, ClientConfig, Credential, DEFAULT_BASE_URL, DEFAULT_WS_URL, Guards, LOCAL_BASE_URL,
    LOCAL_WS_URL,
};
pub use error::{Result, SarvaexError};
pub use models::*;
pub use price::PriceEncoding;
