//! MCP facade: tools plus the Claude Code channel capability.

use crate::config::Trust;
use crate::core::{Core, PostRequest};
use crate::inbox::{Tier, Unread, render_digest, render_event};
use crate::model::{Kind, Priority, Ref};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, CustomNotification, Implementation, InitializeResult, ServerCapabilities,
    ServerNotification,
};
use rmcp::service::{NotificationContext, Peer, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use ulid::Ulid;

pub const CHANNEL_CAPABILITY: &str = "claude/channel";
pub const CHANNEL_NOTIFICATION: &str = "notifications/claude/channel";

#[derive(Clone)]
pub struct WaystationServer {
    core: Arc<Core>,
    peer: Arc<Mutex<Option<Peer<RoleServer>>>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PostParams {
    /// Target channel: `channel` (home realm) or `realm/channel`. `general` reaches everyone;
    /// a repository name reaches every session working in that repository.
    #[serde(default)]
    pub channel: Option<String>,
    /// Post the same message to several channels (e.g. two project channels).
    #[serde(default)]
    pub channels: Vec<String>,
    /// Markdown body.
    pub body: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub priority: Priority,
    /// Agent ids to address directly (they receive it as immediate).
    #[serde(default)]
    pub to: Vec<String>,
    /// Id of the message this replies to.
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub ack_required: bool,
    #[serde(default)]
    pub refs: Vec<Ref>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InboxParams {
    /// Only this realm.
    #[serde(default)]
    pub realm: Option<String>,
    /// Include low-priority / unsubscribed items too.
    #[serde(default)]
    pub include_silent: bool,
    /// Poll the remotes before reading (default true).
    #[serde(default)]
    pub sync: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RegisterParams {
    /// Role label, e.g. `planner`, `reviewer`.
    #[serde(default)]
    pub role: Option<String>,
    /// What you are working on right now.
    #[serde(default)]
    pub focus: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadParams {
    /// Channel to read: `channel` or `realm/channel`.
    pub channel: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentsParams {
    /// Only agents working in this repository (`owner/name` or just `name`).
    #[serde(default)]
    pub project: Option<String>,
    /// Include sessions not seen in the last 10 minutes.
    #[serde(default)]
    pub include_stale: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ThreadParams {
    /// Message id (ULID).
    pub id: String,
}

fn err(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn text(s: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(s)])
}

impl WaystationServer {
    pub fn new(core: Arc<Core>) -> Self {
        Self { core, peer: Arc::new(Mutex::new(None)) }
    }

    fn with_trailer(&self, mut s: String) -> String {
        if let Some(t) = self.core.inbox.lock().unwrap().trailer() {
            s.push_str(&t);
        }
        s
    }

    async fn push(&self, content: String, meta: BTreeMap<String, String>) {
        let guard = self.peer.lock().await;
        let Some(peer) = guard.as_ref() else { return };
        let n = CustomNotification::new(
            CHANNEL_NOTIFICATION,
            Some(serde_json::json!({ "content": content, "meta": meta })),
        );
        if let Err(e) = peer.send_notification(ServerNotification::from(n)).await {
            tracing::warn!("channel push failed: {e}");
        }
    }

    /// Background loop: poll realms, push immediate events, batch the rest.
    async fn run_pusher(self) {
        let poll = self.core.config.poll.clone();
        let mut batch: Vec<Unread> = Vec::new();
        let mut batch_started: Option<Instant> = None;
        let mut last_activity = Instant::now();
        let mut first = true;
        loop {
            if first {
                // Poll right away so anything that arrived between open and initialize is delivered.
                first = false;
            } else {
                let interval = if last_activity.elapsed() > Duration::from_secs(poll.idle_after_secs) {
                    poll.idle_interval_secs
                } else {
                    poll.interval_secs
                };
                // Wake early when the daemon reports a head change.
                let core = self.core.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    core.backend.wait_nudge(Duration::from_secs(interval))
                })
                .await;
            }

            let core = self.core.clone();
            // Under the daemon this is a cheap local diff; standalone it fetches.
            let added = tokio::task::spawn_blocking(move || core.drain_events())
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!(e)));
            let added = match added {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("tick failed: {e:#}");
                    continue;
                }
            };
            if !added.is_empty() {
                last_activity = Instant::now();
            }
            for u in added {
                match u.tier {
                    Tier::Immediate => {
                        let (c, m) = render_event(&u);
                        self.push(c, m).await;
                        self.mark_pushed(&[u]);
                    }
                    Tier::Batched => {
                        batch_started.get_or_insert_with(Instant::now);
                        batch.push(u);
                    }
                    Tier::Silent => {}
                }
            }
            let age_hit = batch_started
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(poll.batch_age_secs));
            if !batch.is_empty() && (batch.len() >= poll.batch_size || age_hit) {
                let (c, m) = render_digest(&batch);
                self.push(c, m).await;
                self.mark_pushed(&batch);
                batch.clear();
                batch_started = None;
            }
        }
    }

    /// Pushed items leave the unread queue (they are in context now).
    fn mark_pushed(&self, items: &[Unread]) {
        let ids: Vec<Ulid> = items.iter().map(|u| u.message.id).collect();
        let mut inbox = self.core.inbox.lock().unwrap();
        inbox.unread.retain(|u| !ids.contains(&u.message.id));
        let _ = inbox.save();
    }
}

#[tool_router]
impl WaystationServer {
    #[tool(
        name = "ws_post",
        description = "Post a message to a Waystation channel. Use `channel` for the home realm or `realm/channel` to name a realm explicitly. Set reply_to to continue a thread."
    )]
    async fn post(&self, Parameters(p): Parameters<PostParams>) -> Result<CallToolResult, McpError> {
        let reply_to = match p.reply_to.as_deref() {
            Some(s) => Some(Ulid::from_string(s).map_err(|e| err(format!("bad reply_to: {e}")))?),
            None => None,
        };
        let mut targets: Vec<String> = p.channel.into_iter().chain(p.channels).collect();
        targets.dedup();
        if targets.is_empty() {
            return Err(err("give `channel` or `channels`"));
        }
        let mut lines = Vec::new();
        for target in targets {
            let req = PostRequest {
                target,
                body: p.body.clone(),
                kind: p.kind,
                priority: p.priority,
                to: p.to.clone(),
                reply_to,
                ack_required: p.ack_required,
                refs: p.refs.clone(),
                tags: p.tags.clone(),
            };
            let core = self.core.clone();
            let posted = tokio::task::spawn_blocking(move || core.post(req))
                .await
                .map_err(err)?
                .map_err(err)?;
            lines.push(format!(
                "posted {} to {}/{} (commit {})",
                posted.message.id,
                posted.realm,
                posted.message.channel,
                &posted.commit[..8.min(posted.commit.len())]
            ));
        }
        Ok(text(self.with_trailer(lines.join("\n"))))
    }

    #[tool(
        name = "ws_inbox",
        description = "Read and clear unread messages from other agents across all mounted realms. Polls the remotes first unless sync=false."
    )]
    async fn inbox(&self, Parameters(p): Parameters<InboxParams>) -> Result<CallToolResult, McpError> {
        if p.sync.unwrap_or(true) {
            let core = self.core.clone();
            tokio::task::spawn_blocking(move || core.tick_all())
                .await
                .map_err(err)?
                .map_err(err)?;
        }
        let items = {
            let mut inbox = self.core.inbox.lock().unwrap();
            let min = if p.include_silent { None } else { Some(Tier::Batched) };
            inbox.drain(p.realm.as_deref(), min)
        };
        let _ = self.core.inbox.lock().unwrap().save();
        if items.is_empty() {
            return Ok(text("inbox empty".into()));
        }
        let mut out = format!("{} unread\n", items.len());
        for u in &items {
            let m = &u.message;
            out.push_str(&format!(
                "\n--- [{}] realm={} trust={} channel={} kind={} priority={} from={}{}\n{}\n",
                m.id,
                u.realm,
                u.trust,
                m.channel,
                m.kind,
                m.priority,
                m.from.agent,
                m.reply_to.map(|r| format!(" reply_to={r}")).unwrap_or_default(),
                m.body.trim_end()
            ));
        }
        Ok(text(out))
    }

    #[tool(
        name = "ws_read",
        description = "Read the most recent messages in a channel (history, not just unread)."
    )]
    async fn read(&self, Parameters(p): Parameters<ReadParams>) -> Result<CallToolResult, McpError> {
        {
            let core = self.core.clone();
            tokio::task::spawn_blocking(move || core.tick_all()).await.map_err(err)?.map_err(err)?;
        }
        let (realm, channel) = self.core.config.resolve_target(&p.channel).map_err(err)?;
        let (realm, channel) = (realm.to_string(), channel.to_string());
        let core = self.core.clone();
        let r2 = realm.clone();
        let msgs = tokio::task::spawn_blocking(move || core.backend.list_messages(&r2, Some(&channel), p.limit))
            .await
            .map_err(err)?
            .map_err(err)?;
        let mut out = format!("{} messages in {}/{}\n", msgs.len(), realm, p.channel);
        for m in msgs.iter().rev() {
            out.push_str(&format!(
                "\n--- [{}] {} kind={} priority={} from={}{}\n{}\n",
                m.id,
                m.ts,
                m.kind,
                m.priority,
                m.from.agent,
                m.reply_to.map(|r| format!(" reply_to={r}")).unwrap_or_default(),
                m.body.trim_end()
            ));
        }
        Ok(text(self.with_trailer(out)))
    }

    #[tool(name = "ws_thread", description = "Fetch one message by id, with its replies.")]
    async fn thread(&self, Parameters(p): Parameters<ThreadParams>) -> Result<CallToolResult, McpError> {
        let id = Ulid::from_string(&p.id).map_err(|e| err(format!("bad id: {e}")))?;
        let core = self.core.clone();
        let found = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<(String, _, Vec<_>)>> {
            for name in core.realms.keys() {
                if let Some(root) = core.backend.find_message(name, id)? {
                    let replies: Vec<_> = core
                        .backend
                        .list_messages(name, Some(&root.channel), 500)?
                        .into_iter()
                        .filter(|m| m.reply_to == Some(id))
                        .collect();
                    return Ok(Some((name.clone(), root, replies)));
                }
            }
            Ok(None)
        })
        .await
        .map_err(err)?
        .map_err(err)?;
        let Some((realm, root, mut replies)) = found else {
            return Ok(text(format!("no message {id} in any mounted realm")));
        };
        replies.sort_by_key(|m| m.id);
        let mut out = format!("realm={} channel={}\n\n[{}] {} {}:\n{}\n", realm, root.channel, root.id, root.ts, root.from.agent, root.body.trim_end());
        for r in replies {
            out.push_str(&format!("\n  ↳ [{}] {} {}:\n{}\n", r.id, r.ts, r.from.agent, r.body.trim_end()));
        }
        Ok(text(self.with_trailer(out)))
    }

    #[tool(
        name = "ws_register",
        description = "Declare this session's role and current focus; writes presence to every mounted realm."
    )]
    async fn register(&self, Parameters(p): Parameters<RegisterParams>) -> Result<CallToolResult, McpError> {
        let core = self.core.clone();
        let res = tokio::task::spawn_blocking(move || core.register(p.role, p.focus, "active"))
            .await
            .map_err(err)?
            .map_err(err)?;
        let lines: Vec<String> = res.iter().map(|(r, sha)| format!("{r}: {}", &sha[..8])).collect();
        Ok(text(format!("registered in {}", lines.join(", "))))
    }

    #[tool(
        name = "ws_agents",
        description = "List agent sessions in each realm and what repository each is working in. Filter with `project` to answer \"is anyone working on X?\"."
    )]
    async fn agents(&self, Parameters(p): Parameters<AgentsParams>) -> Result<CallToolResult, McpError> {
        let core = self.core.clone();
        let all = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, Vec<_>)>> {
            core.realms.keys().map(|n| Ok((n.clone(), core.backend.list_agents(n)?))).collect()
        })
        .await
        .map_err(err)?
        .map_err(err)?;
        let want = p.project.as_deref().map(str::to_lowercase);
        let mut out = String::new();
        let mut shown = 0;
        for (realm, agents) in all {
            out.push_str(&format!("## {realm}\n"));
            for a in agents {
                let active = a.is_active(600);
                if !active && !p.include_stale {
                    continue;
                }
                if let Some(w) = &want
                    && !a.projects.iter().any(|pr| {
                        let pr = pr.to_lowercase();
                        pr == *w || pr.ends_with(&format!("/{w}"))
                    })
                {
                    continue;
                }
                shown += 1;
                out.push_str(&format!(
                    "- {}{} {} projects={} role={} focus={} last_seen={}\n",
                    a.agent,
                    a.session.as_deref().map(|s| format!("@{s}")).unwrap_or_default(),
                    if active { "(active)" } else { "(stale)" },
                    if a.projects.is_empty() { "-".to_string() } else { a.projects.join(",") },
                    a.role.as_deref().unwrap_or("-"),
                    a.focus.as_deref().unwrap_or("-"),
                    a.last_seen
                ));
            }
        }
        if shown == 0 {
            out.push_str("(no matching active sessions)\n");
        }
        Ok(text(self.with_trailer(out)))
    }

    #[tool(name = "ws_realms", description = "List mounted realms, their trust level, and your identity in each.")]
    async fn realms(&self) -> Result<CallToolResult, McpError> {
        let mut out = format!("backend: {}\n", self.core.backend.describe());
        for r in self.core.realms.values() {
            out.push_str(&format!(
                "- {} trust={} agent={} subscriptions={:?} remote={}\n",
                r.name, r.trust, r.agent_id, r.subscriptions, r.remote
            ));
        }
        Ok(text(out))
    }

    #[tool(name = "ws_sync", description = "Fetch all realms now and report what arrived.")]
    async fn sync(&self) -> Result<CallToolResult, McpError> {
        let core = self.core.clone();
        let added = tokio::task::spawn_blocking(move || core.tick_all())
            .await
            .map_err(err)?
            .map_err(err)?;
        Ok(text(self.with_trailer(format!("synced; {} new", added.len()))))
    }
}

#[tool_handler]
impl ServerHandler for WaystationServer {
    fn get_info(&self) -> InitializeResult {
        let mut experimental = BTreeMap::new();
        experimental.insert(CHANNEL_CAPABILITY.to_string(), serde_json::Map::new());
        let caps = ServerCapabilities::builder()
            .enable_tools()
            .enable_experimental_with(experimental)
            .build();
        let mut info = InitializeResult::new(caps);
        info.server_info = Implementation::new("waystation", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(self.core.instructions());
        info
    }

    async fn on_initialized(&self, ctx: NotificationContext<RoleServer>) {
        *self.peer.lock().await = Some(ctx.peer.clone());
        // Presence is automatic: announce this session in every realm.
        let core = self.core.clone();
        let role = std::env::var("WAYSTATION_ROLE").ok();
        let focus = std::env::var("WAYSTATION_FOCUS").ok();
        tokio::spawn(async move {
            if let Err(e) = tokio::task::spawn_blocking(move || core.register(role, focus, "active")).await {
                tracing::warn!("auto-register failed: {e}");
            }
        });
        let external = self.core.realms.values().filter(|r| r.trust == Trust::External).count();
        tracing::info!(realms = self.core.realms.len(), external, "client initialized; starting pusher");
        tokio::spawn(self.clone().run_pusher());
    }
}
