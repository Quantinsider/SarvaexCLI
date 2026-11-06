# Multi-stage build for the `sarvaex` binary (CLI + stdio MCP + hosted MCP server).
# Build:  docker build -f deploy/Dockerfile -t sarvaex/sarvaex:latest .
# Run:    docker run --rm sarvaex/sarvaex markets list --base-url https://api.sarvaex.com

FROM rust:1.89-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake clang pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY SKILL.md ./SKILL.md
RUN cargo build --release -p sarvaex-cli -p sarvaex-mock-gateway \
    && strip target/release/sarvaex target/release/sarvaex-mock-gateway

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/sarvaex-mcp --create-home sarvaex
COPY --from=builder /app/target/release/sarvaex /usr/local/bin/sarvaex
COPY --from=builder /app/target/release/sarvaex-mock-gateway /usr/local/bin/sarvaex-mock-gateway
USER sarvaex
WORKDIR /var/lib/sarvaex-mcp
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/sarvaex"]
CMD ["--help"]
