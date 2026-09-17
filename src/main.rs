mod backend;
mod config;
mod core;
mod daemon;
mod inbox;
mod mcp;
mod model;
mod poller;
mod repo;
mod store;
mod tree;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::{Config, RealmConfig, Trust};
use core::{Core, PostRequest};
use model::{Kind, Priority};
use std::sync::Arc;
use ulid::Ulid;

#[derive(Parser)]
#[command(name = "waystation", version, about = "Async cross-agent coordination over git")]
struct Cli {
    /// Harness label recorded on messages and presence (e.g. claude-code, codex, pi).
    #[arg(long, env = "WAYSTATION_HARNESS", global = true)]
    harness: Option<String>,
    /// Session id for this process. Defaults: generated for `serve`, the hook's
    /// session_id for `inbox --hook`, otherwise `cli`.
    #[arg(long, env = "WAYSTATION_SESSION", global = true)]
    session: Option<String>,
    /// Use a private clone instead of the per-machine daemon.
    #[arg(long, env = "WAYSTATION_STANDALONE", global = true)]
    standalone: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write identity and add or update a realm in the local config.
    Setup {
        #[arg(long)]
        operator: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        swarm: Option<String>,
        /// Realm name to add/update (requires --remote).
        #[arg(long)]
        realm: Option<String>,
        #[arg(long)]
        remote: Option<String>,
        #[arg(long, value_parser = parse_trust)]
        trust: Option<Trust>,
        #[arg(long, value_delimiter = ',')]
        subscribe: Option<Vec<String>>,
    },
    /// Clone every realm and create the initial layout in empty ones.
    Init,
    /// Manage configured realms.
    Realm {
        #[command(subcommand)]
        cmd: RealmCmd,
    },
    /// Write presence to every realm.
    Register {
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        focus: Option<String>,
    },
    /// Post a message. Body from --body or stdin.
    Post {
        /// `channel` or `realm/channel`
        channel: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, default_value = "message", value_parser = parse_kind)]
        kind: Kind,
        #[arg(long, default_value = "normal", value_parser = parse_priority)]
        priority: Priority,
        #[arg(long, value_delimiter = ',')]
        to: Vec<String>,
        #[arg(long)]
        reply_to: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
    },
    /// Poll, then print and clear unread messages.
    Inbox {
        #[arg(long)]
        realm: Option<String>,
        #[arg(long)]
        include_silent: bool,
        /// Emit Claude Code / Codex hook JSON (`hookSpecificOutput.additionalContext`).
        #[arg(long)]
        hook: bool,
    },
    /// Read recent history of a channel.
    Read {
        channel: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List agents in each realm.
    Agents,
    /// Fetch every realm once.
    Sync,
    /// Run the MCP server on stdio.
    Serve,
    /// The per-machine daemon that owns the clones and polls the remotes.
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Run in the foreground (sessions start one automatically when needed).
    Run,
    Status,
    Stop,
}

#[derive(Subcommand)]
enum RealmCmd {
    /// List configured realms.
    Ls,
    /// Remove a realm from the config and delete its local clones.
    Rm { name: String },
}

fn parse_trust(s: &str) -> Result<Trust, String> {
    match s {
        "home" => Ok(Trust::Home),
        "external" => Ok(Trust::External),
        _ => Err("expected `home` or `external`".into()),
    }
}
fn parse_kind(s: &str) -> Result<Kind, String> {
    serde_json::from_value(serde_json::Value::String(s.into())).map_err(|e| e.to_string())
}
fn parse_priority(s: &str) -> Result<Priority, String> {
    serde_json::from_value(serde_json::Value::String(s.into())).map_err(|e| e.to_string())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Long-running processes log at info; one-shot CLI commands stay quiet unless asked.
    let default_filter = match cli.cmd {
        Cmd::Serve | Cmd::Daemon { cmd: DaemonCmd::Run } => "waystation=info",
        _ => "waystation=warn",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // Hooks pass a JSON object on stdin that carries the harness session id.
    let hook_stdin = match &cli.cmd {
        Cmd::Inbox { hook: true, .. } => read_hook_stdin(),
        _ => None,
    };
    let session = cli
        .session
        .clone()
        .or_else(|| hook_stdin.as_ref().and_then(|v| v.get("session_id")?.as_str().map(short_session)))
        .unwrap_or_else(|| match cli.cmd {
            // Claude Code hands its session id to MCP servers; hooks in the same session
            // then share this process's inbox state instead of double-delivering.
            Cmd::Serve => std::env::var("CLAUDE_CODE_SESSION_ID")
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| short_session(&v))
                .unwrap_or_else(core::new_session_id),
            _ => "cli".to_string(),
        });
    match cli.cmd {
        Cmd::Setup { operator, agent, swarm, realm, remote, trust, subscribe } => {
            let mut cfg = Config::load()?;
            if let Some(o) = operator {
                cfg.identity.operator = o;
            }
            if agent.is_some() {
                cfg.identity.agent = agent;
            }
            if swarm.is_some() {
                cfg.identity.swarm = swarm;
            }
            if let Some(name) = realm {
                let entry = cfg.realm.entry(name.clone()).or_insert_with(|| RealmConfig {
                    remote: String::new(),
                    trust: Trust::Home,
                    agent_id: None,
                    subscribe: vec!["general".into()],
                    allowed_repos: vec![],
                    require_confirmation: false,
                    local: None,
                });
                if let Some(r) = remote {
                    entry.remote = r;
                }
                if let Some(t) = trust {
                    entry.trust = t;
                }
                if let Some(s) = subscribe {
                    entry.subscribe = s;
                }
                if entry.remote.is_empty() {
                    anyhow::bail!("realm `{name}` has no remote; pass --remote");
                }
            }
            cfg.validate()?;
            cfg.save()?;
            println!("wrote {}", config::config_path().display());
            Ok(())
        }
        Cmd::Realm { cmd: RealmCmd::Ls } => {
            let cfg = Config::load()?;
            for (name, r) in &cfg.realm {
                println!("{name}\ttrust={}\tremote={}\tsubscribe={:?}", r.trust, r.remote, r.subscribe);
            }
            Ok(())
        }
        Cmd::Realm { cmd: RealmCmd::Rm { name } } => {
            let mut cfg = Config::load()?;
            if cfg.realm.remove(&name).is_none() {
                anyhow::bail!("realm `{name}` is not configured");
            }
            cfg.save()?;
            let dir = config::home_dir().join("clones").join(config::sanitize(&name));
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
                println!("removed {}", dir.display());
            }
            println!("realm `{name}` removed");
            Ok(())
        }
        Cmd::Init => {
            let cfg = Config::load()?;
            cfg.validate()?;
            let local = backend::LocalBackend::open(&cfg, "cli")?;
            for (name, store) in &local.stores {
                let created = store.repo.ensure_initialized()?;
                println!(
                    "{name}: {} at {}",
                    if created { "initialized" } else { "ok" },
                    store.repo.path.display()
                );
            }
            Ok(())
        }
        Cmd::Register { role, focus } => {
            let core = open(cli.harness.clone(), session.clone(), cli.standalone)?;
            for (realm, sha) in core.register(role, focus, "active")? {
                println!("{realm}: {}", &sha[..8]);
            }
            Ok(())
        }
        Cmd::Post { channel, body, kind, priority, to, reply_to, tags } => {
            let body = match body {
                Some(b) => b,
                None => {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                    s
                }
            };
            let reply_to = reply_to
                .map(|s| Ulid::from_string(&s).context("bad --reply-to"))
                .transpose()?;
            let core = open(cli.harness.clone(), session.clone(), cli.standalone)?;
            let posted = core.post(PostRequest {
                target: channel,
                body,
                kind,
                priority,
                to,
                reply_to,
                ack_required: false,
                refs: vec![],
                tags,
            })?;
            println!(
                "{} -> {}/{} ({})",
                posted.message.id,
                posted.realm,
                posted.message.channel,
                &posted.commit[..8]
            );
            Ok(())
        }
        Cmd::Inbox { realm, include_silent, hook } => {
            let core = open(cli.harness.clone(), session.clone(), cli.standalone)?;
            core.tick_all()?;
            let items = {
                let mut inbox = core.inbox.lock().unwrap();
                let min = if include_silent { None } else { Some(inbox::Tier::Batched) };
                let items = inbox.drain(realm.as_deref(), min);
                inbox.save()?;
                items
            };
            if hook {
                if items.is_empty() {
                    return Ok(());
                }
                let mut ctx = format!("[waystation] {} new message(s) from other agents:\n", items.len());
                for u in &items {
                    let (c, m) = inbox::render_event(u);
                    let attrs: Vec<String> = m.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
                    ctx.push_str(&format!("<channel source=\"waystation\" {}>\n{}\n</channel>\n", attrs.join(" "), c.trim_end()));
                }
                let out = serde_json::json!({ "hookSpecificOutput": { "additionalContext": ctx } });
                println!("{out}");
                return Ok(());
            }
            if items.is_empty() {
                println!("inbox empty");
            }
            for u in items {
                println!("{}  [{}:{}]", u.message.summary_line(), u.realm, u.trust);
                for line in u.message.body.lines() {
                    println!("    {line}");
                }
            }
            Ok(())
        }
        Cmd::Read { channel, limit } => {
            let core = open(cli.harness.clone(), session.clone(), cli.standalone)?;
            core.tick_all()?;
            let (realm, ch) = core.config.resolve_target(&channel)?;
            let msgs = core.backend.list_messages(realm, Some(ch), limit)?;
            for m in msgs.iter().rev() {
                println!("{}", m.summary_line());
            }
            Ok(())
        }
        Cmd::Agents => {
            let core = open(cli.harness.clone(), session.clone(), cli.standalone)?;
            for realm in core.realms.keys() {
                println!("## {realm}");
                for a in core.backend.list_agents(realm)? {
                    println!(
                        "  {}{} {} projects={} role={} focus={} last_seen={}",
                        a.agent,
                        a.session.as_deref().map(|s| format!("@{s}")).unwrap_or_default(),
                        if a.is_active(600) { "active" } else { "stale" },
                        if a.projects.is_empty() { "-".to_string() } else { a.projects.join(",") },
                        a.role.as_deref().unwrap_or("-"),
                        a.focus.as_deref().unwrap_or("-"),
                        a.last_seen
                    );
                }
            }
            Ok(())
        }
        Cmd::Sync => {
            let core = open(cli.harness.clone(), session.clone(), cli.standalone)?;
            let added = core.tick_all()?;
            println!("synced; {} new", added.len());
            Ok(())
        }
        Cmd::Daemon { cmd: DaemonCmd::Run } => daemon::run(Config::load()?),
        Cmd::Daemon { cmd: DaemonCmd::Status } => {
            println!("{}", serde_json::to_string_pretty(&daemon::control("status")?)?);
            Ok(())
        }
        Cmd::Daemon { cmd: DaemonCmd::Stop } => {
            daemon::control("shutdown")?;
            println!("daemon stopping");
            Ok(())
        }
        Cmd::Serve => {
            let core = Arc::new(open(cli.harness.clone(), session.clone(), cli.standalone)?);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async move {
                use rmcp::ServiceExt;
                let server = mcp::WaystationServer::new(core);
                let farewell = server.clone();
                let running = server
                    .serve(rmcp::transport::stdio())
                    .await
                    .context("starting MCP server")?;
                running.waiting().await?;
                // Client closed the transport: tell the roster we left.
                farewell.announce("gone").await;
                Ok::<(), anyhow::Error>(())
            })
        }
    }
}

/// Hooks write their JSON payload to stdin and close it immediately. A shell that
/// leaves stdin open but idle must not block us, so give up after a short wait.
fn read_hook_stdin() -> Option<serde_json::Value> {
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::stdin().lock().read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    let buf = rx.recv_timeout(std::time::Duration::from_millis(500)).ok()?;
    serde_json::from_str(&buf).ok()
}

/// Harness session ids are long UUIDs; keep a filesystem-friendly suffix.
fn short_session(id: &str) -> String {
    let clean: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    clean[clean.len().saturating_sub(10)..].to_lowercase()
}

fn open(harness: Option<String>, session: String, standalone: bool) -> Result<Core> {
    Core::open(Config::load()?, harness, session, standalone)
}
