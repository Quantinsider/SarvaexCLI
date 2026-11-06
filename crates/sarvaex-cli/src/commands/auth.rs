use super::Ctx;
use crate::cli::{AuthCmd, KeysCmd, LoginArgs, RegisterArgs};
use crate::config;
use crate::output::{exit, kv, table};
use anyhow::{Context, Result, anyhow, bail};
use sarvaex_sdk::{ALL_SCOPES, Credential, READ_SCOPES};
use serde_json::json;
use std::io::Read;

pub async fn run(ctx: &Ctx, cmd: AuthCmd) -> Result<i32> {
    match cmd {
        AuthCmd::Login(args) => login(ctx, args).await,
        AuthCmd::Register(args) => register(ctx, args).await,
        AuthCmd::SetKey { key } => set_key(ctx, key).await,
        AuthCmd::Whoami => whoami(ctx).await,
        AuthCmd::Status => status(ctx),
        AuthCmd::Logout { revoke } => logout(ctx, revoke).await,
        AuthCmd::Keys(k) => keys(ctx, k).await,
    }
}

fn read_password(ctx: &Ctx, from_stdin: bool, prompt: &str) -> Result<String> {
    if let Ok(p) = std::env::var("SARVAEX_PASSWORD")
        && !p.is_empty()
    {
        return Ok(p);
    }
    if from_stdin || !ctx.out.interactive() {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading password from stdin")?;
        let s = s.trim_end_matches(['\n', '\r']).to_owned();
        if s.is_empty() {
            bail!(
                "no password on stdin (use --password-stdin, SARVAEX_PASSWORD, or run interactively)"
            );
        }
        return Ok(s);
    }
    rpassword::prompt_password(prompt).context("reading password")
}

fn read_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_owned())
}

async fn login(ctx: &Ctx, args: LoginArgs) -> Result<i32> {
    let identifier = match args.identifier {
        Some(i) => i,
        None if ctx.out.interactive() => read_line("Email or user id: ")?,
        None => bail!("--identifier is required when not running interactively"),
    };
    let password = read_password(ctx, args.password_stdin, "Password: ")?;

    let anon = ctx.client().with_swapped_credential(Credential::None);
    let login = anon
        .login(&identifier, &password)
        .await
        .context("login failed")?;
    let session = anon.with_swapped_credential(Credential::Bearer(login.token.clone()));

    let scopes: Vec<&str> = if args.read_only {
        READ_SCOPES
            .iter()
            .copied()
            .chain(["websocket:read"])
            .collect()
    } else {
        ALL_SCOPES.to_vec()
    };
    let expires_at = args
        .expires_in_days
        .map(|d| (chrono::Utc::now() + chrono::Duration::days(d as i64)).to_rfc3339());
    let key_name = format!("{}@{}", args.key_name, hostname());
    let created = session
        .create_api_key(&key_name, &scopes, expires_at.as_deref())
        .await
        .context("creating API key")?;

    let user_id = if login.user_id.is_empty() {
        session
            .profile()
            .await
            .map(|p| p.user_id)
            .unwrap_or_default()
    } else {
        login.user_id.clone()
    };

    if args.no_store {
        ctx.out.emit(
            &json!({ "user_id": user_id, "key": created.key, "key_id": created.key_id, "key_prefix": created.key_prefix, "scopes": created.scopes, "expires_at": created.expires_at }),
            || format!("API key for {user_id} (store it now, it will not be shown again):\n{}", created.key),
        );
        return Ok(exit::OK);
    }

    let path = config::store_credential(
        &ctx.resolved.profile_name,
        &ctx.resolved.base_url,
        &created.key,
        Some(&user_id),
        Some(&created.key_id),
    )?;
    ctx.out.emit(
        &json!({ "ok": true, "profile": ctx.resolved.profile_name, "user_id": user_id, "key_prefix": created.key_prefix, "scopes": created.scopes, "expires_at": created.expires_at, "config": path }),
        || {
            format!(
                "Logged in as {user_id}. Created API key {} ({}) and saved it to profile '{}' in {}.\nScopes: {}",
                created.key_prefix,
                key_name,
                ctx.resolved.profile_name,
                path.display(),
                created.scopes.join(", ")
            )
        },
    );
    Ok(exit::OK)
}

async fn register(ctx: &Ctx, args: RegisterArgs) -> Result<i32> {
    let password = read_password(ctx, args.password_stdin, "Choose a password: ")?;
    let anon = ctx.client().with_swapped_credential(Credential::None);
    let resp = anon
        .register(&args.name, &args.user_id, &args.email, &password)
        .await
        .context("registration failed")?;
    if args.login {
        let session = anon.with_swapped_credential(Credential::Bearer(resp.token.clone()));
        let created = session
            .create_api_key(&format!("sarvaex-cli@{}", hostname()), &ALL_SCOPES, None)
            .await?;
        let path = config::store_credential(
            &ctx.resolved.profile_name,
            &ctx.resolved.base_url,
            &created.key,
            Some(&args.user_id),
            Some(&created.key_id),
        )?;
        ctx.out.emit(
            &json!({ "ok": true, "user_id": args.user_id, "email": args.email, "key_prefix": created.key_prefix, "config": path }),
            || format!("Registered {} and saved API key {} to profile '{}'.", args.user_id, created.key_prefix, ctx.resolved.profile_name),
        );
    } else {
        ctx.out.emit(
            &json!({ "ok": true, "user_id": resp.user_id, "email": resp.email, "name": resp.name }),
            || {
                format!(
                    "Registered {}. Run `sarvaex auth login` to create an API key.",
                    resp.user_id
                )
            },
        );
    }
    Ok(exit::OK)
}

async fn set_key(ctx: &Ctx, key: Option<String>) -> Result<i32> {
    let key = match key {
        Some(k) => k,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s.trim().to_owned()
        }
    };
    if !key.starts_with("svx_") {
        bail!("that does not look like a SarvaEX API key (expected svx_live_…)");
    }
    let probe = ctx
        .client()
        .with_swapped_credential(Credential::ApiKey(key.clone()));
    let profile = probe
        .profile()
        .await
        .context("the key was rejected by the exchange")?;
    let path = config::store_credential(
        &ctx.resolved.profile_name,
        &ctx.resolved.base_url,
        &key,
        Some(&profile.user_id),
        None,
    )?;
    ctx.out.emit(
        &json!({ "ok": true, "profile": ctx.resolved.profile_name, "user_id": profile.user_id, "config": path }),
        || format!("Stored key for {} in profile '{}'.", profile.user_id, ctx.resolved.profile_name),
    );
    Ok(exit::OK)
}

async fn whoami(ctx: &Ctx) -> Result<i32> {
    let p = ctx.client().profile().await?;
    ctx.out.emit(&p, || {
        kv(&[
            ("user_id", p.user_id.clone()),
            ("name", p.name.clone()),
            ("email", p.email.clone()),
            ("status", p.status.clone()),
            ("created", p.created_at.clone().unwrap_or_default()),
        ])
    });
    Ok(exit::OK)
}

fn status(ctx: &Ctx) -> Result<i32> {
    let r = &ctx.resolved;
    let guards = r.guards_summary();
    ctx.out.emit(
        &json!({
            "profile": r.profile_name,
            "base_url": r.base_url,
            "ws_url": r.ws_url,
            "credential": r.credential.redacted(),
            "credential_type": match r.credential { Credential::None => "none", Credential::ApiKey(_) => "api_key", Credential::Bearer(_) => "bearer" },
            "user_id": r.profile.user_id,
            "guards": guards,
            "config": config::config_path(),
        }),
        || {
            kv(&[
                ("profile", r.profile_name.clone()),
                ("base_url", r.base_url.clone()),
                ("credential", r.credential.redacted()),
                ("user_id", r.profile.user_id.clone().unwrap_or_default()),
                ("guards", if guards.is_empty() { "none".into() } else { guards.join("; ") }),
                ("config", config::config_path().display().to_string()),
            ])
        },
    );
    Ok(exit::OK)
}

async fn logout(ctx: &Ctx, revoke: bool) -> Result<i32> {
    let old = config::clear_credential(&ctx.resolved.profile_name)?;
    let mut revoked = false;
    if revoke
        && let Some(p) = &old
        && let (Some(key), Some(id)) = (&p.api_key, &p.key_id)
    {
        let c = ctx
            .client()
            .with_swapped_credential(Credential::ApiKey(key.clone()));
        revoked = c.revoke_api_key(id).await.is_ok();
    }
    ctx.out.emit(
        &json!({ "ok": true, "profile": ctx.resolved.profile_name, "revoked": revoked }),
        || {
            format!(
                "Removed credential from profile '{}'{}.",
                ctx.resolved.profile_name,
                if revoked {
                    " and revoked it on the exchange"
                } else {
                    ""
                }
            )
        },
    );
    Ok(exit::OK)
}

async fn keys(ctx: &Ctx, cmd: KeysCmd) -> Result<i32> {
    match cmd {
        KeysCmd::List => {
            let keys = ctx.client().list_api_keys().await?;
            ctx.out.emit(&json!({ "api_keys": keys }), || {
                let mut t = table(&[
                    "key_id",
                    "prefix",
                    "name",
                    "scopes",
                    "created",
                    "expires",
                    "last used",
                    "revoked",
                ]);
                for k in &keys {
                    t.add_row(vec![
                        k.key_id.clone(),
                        k.key_prefix.clone(),
                        k.name.clone(),
                        k.scopes.join(","),
                        crate::output::short_ts(&k.created_at),
                        crate::output::short_ts(&k.expires_at),
                        crate::output::short_ts(&k.last_used_at),
                        crate::output::short_ts(&k.revoked_at),
                    ]);
                }
                t.to_string()
            });
            Ok(exit::OK)
        }
        KeysCmd::Create {
            name,
            scopes,
            expires_at,
        } => {
            let scopes: Vec<String> =
                scopes.unwrap_or_else(|| ALL_SCOPES.iter().map(|s| s.to_string()).collect());
            for s in &scopes {
                if !ALL_SCOPES.contains(&s.as_str()) {
                    return Err(anyhow!(
                        "unsupported scope '{s}'; allowed: {}",
                        ALL_SCOPES.join(", ")
                    ));
                }
            }
            let refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
            let created = ctx
                .client()
                .create_api_key(&name, &refs, expires_at.as_deref())
                .await?;
            ctx.out.emit(&created, || {
                format!(
                    "Created key {} ({}). Secret (shown once):\n{}\nScopes: {}",
                    created.key_prefix,
                    created.name,
                    created.key,
                    created.scopes.join(", ")
                )
            });
            Ok(exit::OK)
        }
        KeysCmd::Revoke { key_id } => {
            let v = ctx.client().revoke_api_key(&key_id).await?;
            ctx.out.emit(
                &json!({ "ok": true, "key_id": key_id, "response": v }),
                || format!("Revoked {key_id}."),
            );
            Ok(exit::OK)
        }
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "host".into())
}
