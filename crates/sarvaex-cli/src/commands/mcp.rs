use super::Ctx;
use crate::cli::{McpCmd, ServeArgs};
use crate::output::exit;
use anyhow::{Context, Result, bail};
use sarvaex_mcp::{McpState, RemoteOptions};
use sarvaex_sdk::Credential;
use serde_json::json;
use std::sync::Arc;

fn environment_label(base_url: &str) -> String {
    if base_url.contains("localhost") || base_url.contains("127.0.0.1") {
        "local".into()
    } else if base_url.contains("api.sarvaex.com") {
        "production".into()
    } else {
        base_url.trim_end_matches('/').to_owned()
    }
}

pub async fn run(ctx: &Ctx, cmd: McpCmd) -> Result<i32> {
    match cmd {
        McpCmd::Stdio => {
            if !ctx.client().credential().is_some() {
                eprintln!(
                    "sarvaex mcp: no credential configured — market-data tools will work, account/trading tools will not. Set SARVAEX_API_KEY or run `sarvaex auth login`."
                );
            }
            let state = Arc::new(McpState {
                base: ctx.client().clone(),
                environment_label: environment_label(&ctx.resolved.base_url),
            });
            sarvaex_mcp::serve_stdio(state).await?;
            Ok(exit::OK)
        }
        McpCmd::Serve(args) => serve(ctx, args).await,
        McpCmd::Config { target, with_key } => config(ctx, &target, with_key),
    }
}

async fn serve(ctx: &Ctx, args: ServeArgs) -> Result<i32> {
    let public_url = url::Url::parse(&args.public_url)
        .context("--public-url must be an absolute URL, e.g. https://mcp.sarvaex.com")?;
    if public_url.scheme() != "https"
        && !public_url
            .host_str()
            .is_some_and(|h| h == "localhost" || h == "127.0.0.1")
    {
        eprintln!(
            "warning: public URL is not https; Claude.ai and ChatGPT require https for custom connectors"
        );
    }
    let bind: std::net::SocketAddr = args.bind.parse().context("--bind must be host:port")?;
    let state_dir = args
        .state_dir
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::config::config_dir().join("mcp"));
    // The remote server never uses the operator's own credential; each request carries the user's.
    let base = ctx.client().with_swapped_credential(Credential::None);
    let state = Arc::new(McpState {
        base,
        environment_label: environment_label(&ctx.resolved.base_url),
    });
    let opts = RemoteOptions {
        bind,
        public_url,
        state_dir,
        allowed_hosts: args.allowed_host,
        allow_any_host: args.allow_any_host,
        allow_password_login: !args.no_password_login,
        allow_api_key_paste: !args.no_api_key_paste,
    };
    if !opts.allow_password_login && !opts.allow_api_key_paste {
        bail!("at least one of password login or API-key paste must stay enabled");
    }
    sarvaex_mcp::remote::serve(state, opts).await?;
    Ok(exit::OK)
}

fn config(ctx: &Ctx, target: &str, with_key: bool) -> Result<i32> {
    let key = match (&ctx.resolved.credential, with_key) {
        (Credential::ApiKey(k), true) => k.clone(),
        _ => "svx_live_REPLACE_ME".to_owned(),
    };
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "sarvaex".into());
    let mut env = json!({ "SARVAEX_API_KEY": key });
    if ctx.resolved.base_url != sarvaex_sdk::DEFAULT_BASE_URL {
        env["SARVAEX_BASE_URL"] = json!(ctx.resolved.base_url);
    }
    match target {
        "claude-desktop" | "cursor" | "windsurf" | "json" => {
            let snippet = json!({ "mcpServers": { "sarvaex": { "command": exe, "args": ["mcp", "stdio"], "env": env } } });
            ctx.out.json_out(&snippet);
            if !ctx.out.json {
                let path = match target {
                    "claude-desktop" => {
                        "Claude Desktop → Settings → Developer → Edit Config (claude_desktop_config.json)"
                    }
                    "cursor" => "~/.cursor/mcp.json",
                    "windsurf" => "~/.codeium/windsurf/mcp_config.json",
                    _ => "your client's MCP config",
                };
                eprintln!("\nAdd the above to {path}. Restart the client afterwards.");
            }
        }
        "claude-code" => {
            let mut cmd = format!("claude mcp add sarvaex -e SARVAEX_API_KEY={key}");
            if let Some(b) = env.get("SARVAEX_BASE_URL").and_then(|v| v.as_str()) {
                cmd.push_str(&format!(" -e SARVAEX_BASE_URL={b}"));
            }
            cmd.push_str(&format!(" -- {exe} mcp stdio"));
            ctx.out.emit(&json!({ "command": cmd }), || {
                format!("{cmd}\n\nAlso install the skill into your project: sarvaex docs install")
            });
        }
        "remote" => {
            ctx.out.emit(
                &json!({
                    "claude_ai": "Settings → Connectors → Add custom connector → URL: https://<your-mcp-host>/mcp",
                    "chatgpt": "Settings → Connectors → Create → URL: https://<your-mcp-host>/mcp, Authentication: OAuth",
                    "claude_code": "claude mcp add --transport http sarvaex https://<your-mcp-host>/mcp",
                    "run_server": "sarvaex mcp serve --public-url https://<your-mcp-host> --bind 0.0.0.0:8787"
                }),
                || {
                    "Run the server:   sarvaex mcp serve --public-url https://<your-mcp-host> --bind 0.0.0.0:8787\n\
                     Claude.ai:        Settings → Connectors → Add custom connector → https://<your-mcp-host>/mcp\n\
                     ChatGPT:          Settings → Connectors → Create → https://<your-mcp-host>/mcp (OAuth)\n\
                     Claude Code:      claude mcp add --transport http sarvaex https://<your-mcp-host>/mcp"
                        .to_owned()
                },
            );
        }
        other => bail!(
            "unknown target '{other}' (claude-desktop | claude-code | cursor | windsurf | remote)"
        ),
    }
    Ok(exit::OK)
}
