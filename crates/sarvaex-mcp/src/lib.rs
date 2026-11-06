//! # sarvaex-mcp
//!
//! Model Context Protocol server for the SarvaEX exchange.
//!
//! * [`serve_stdio`] — local server for Claude Desktop, Claude Code, Cursor, etc.
//!   Uses the credential configured in `McpState`.
//! * [`remote::serve`] — hosted Streamable HTTP server with OAuth 2.1 so that
//!   Claude.ai and ChatGPT can add SarvaEX as a custom connector and every
//!   user trades with their own account.

pub mod remote;
pub mod store;
pub mod tools;

pub use remote::{RemoteOptions, build_app};
pub use tools::{INSTRUCTIONS, McpState, Principal, SarvaexMcp};

/// The agent-facing trading rules (also shipped as `SKILL.md`).
pub const SKILL_MD: &str = include_str!("../../../SKILL.md");

use std::sync::Arc;

/// Serve MCP over stdio until the client disconnects.
pub async fn serve_stdio(state: Arc<McpState>) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let service = SarvaexMcp::new(state)
        .serve(rmcp::transport::stdio())
        .await?;
    let reason = service.waiting().await?;
    tracing::info!(?reason, "stdio MCP session ended");
    Ok(())
}
