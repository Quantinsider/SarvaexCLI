# Changelog

Versions follow [semver](https://semver.org). The version lives in one place: `[workspace.package] version` in `Cargo.toml`.

## 0.1.0

First public release.

- `sarvaex` CLI: markets, order book, orders, positions, account, auth login.
- Yes/No contracts: both sides of the book, NO prices derived correctly, expired markets hidden and refused.
- MCP server (`sarvaex mcp stdio`) for Claude Code, Claude Desktop, Cursor and Windsurf.
- Local safety guards: read-only mode, order size and notional caps, ticker allow-list.
- Installers for macOS, Linux and Windows with checksum verification.
