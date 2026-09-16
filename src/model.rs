//! Repository data model: messages and presence files.
//!
//! Every file is Markdown with a YAML frontmatter block. See DESIGN.md §3.

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Message,
    Finding,
    Decision,
    Request,
    Status,
    Escalation,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self).unwrap();
        f.write_str(s.as_str().unwrap())
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self).unwrap();
        f.write_str(s.as_str().unwrap())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Author {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq, schemars::JsonSchema)]
pub struct Ref {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub id: Ulid,
    pub ts: Timestamp,
    pub from: Author,
    pub channel: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Ulid>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ack_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub body: String,
}

impl Message {
    pub fn new(from: Author, channel: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            id: Ulid::generate(),
            ts: Timestamp::now(),
            from,
            channel: channel.into(),
            kind: Kind::Message,
            priority: Priority::Normal,
            to: Vec::new(),
            reply_to: None,
            ack_required: false,
            refs: Vec::new(),
            tags: Vec::new(),
            body: body.into(),
        }
    }

    /// Path inside the realm repo.
    pub fn path(&self) -> String {
        format!("channels/{}/{}.md", self.channel, self.id)
    }

    pub fn to_markdown(&self) -> Result<String> {
        to_frontmatter_doc(self, "body", &self.body)
    }

    pub fn from_markdown(text: &str) -> Result<Self> {
        let (mut msg, body): (Message, String) = from_frontmatter_doc(text)?;
        msg.body = body;
        Ok(msg)
    }

    /// True when the path is a channel message file.
    pub fn is_message_path(path: &str) -> bool {
        path.starts_with("channels/") && path.ends_with(".md")
    }

    pub fn summary_line(&self) -> String {
        let first = self.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        let first: String = first.chars().take(120).collect();
        format!(
            "[{}] {} {}/{} {}: {}",
            self.id, self.priority, self.channel, self.kind, self.from.agent, first
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Presence {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    pub last_seen: Timestamp,
    /// Repositories this session is working in, as `owner/name`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

impl Presence {
    pub fn path(&self) -> String {
        Self::path_for(&self.agent, self.session.as_deref())
    }

    /// `agents/<agent>/<session>.md`, or `agents/<agent>.md` for a session-less agent.
    pub fn path_for(agent: &str, session: Option<&str>) -> String {
        match session {
            Some(s) => format!("agents/{agent}/{s}.md"),
            None => format!("agents/{agent}.md"),
        }
    }

    pub fn is_presence_path(path: &str) -> bool {
        path.starts_with("agents/") && path.ends_with(".md")
    }

    pub fn to_markdown(&self) -> Result<String> {
        to_frontmatter_doc(self, "notes", &self.notes)
    }

    pub fn from_markdown(text: &str) -> Result<Self> {
        let (mut p, notes): (Presence, String) = from_frontmatter_doc(text)?;
        p.notes = notes;
        Ok(p)
    }

    pub fn is_active(&self, heartbeat_secs: i64) -> bool {
        if self.status == "gone" {
            return false;
        }
        let age = Timestamp::now().as_second() - self.last_seen.as_second();
        age < heartbeat_secs * 2
    }
}

fn to_frontmatter_doc<T: Serialize>(header: &T, body_key: &str, body: &str) -> Result<String> {
    let mut value = serde_yaml_ng::to_value(header)?;
    if let serde_yaml_ng::Value::Mapping(m) = &mut value {
        m.remove(serde_yaml_ng::Value::String(body_key.to_string()));
    }
    let yaml = serde_yaml_ng::to_string(&value)?;
    let mut out = String::with_capacity(yaml.len() + body.len() + 16);
    out.push_str("---\n");
    out.push_str(&yaml);
    if !yaml.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n\n");
    out.push_str(body.trim_end());
    out.push('\n');
    Ok(out)
}

fn from_frontmatter_doc<T: for<'de> Deserialize<'de>>(text: &str) -> Result<(T, String)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .context("missing frontmatter opening `---`")?;
    let end = rest
        .find("\n---")
        .context("missing frontmatter closing `---`")?;
    let yaml = &rest[..end];
    let after = &rest[end + 4..];
    let body = after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after);
    if !after.is_empty() && !after.starts_with('\n') && !after.starts_with("\r\n") {
        bail!("frontmatter closing `---` must end its line");
    }
    let header: T = serde_yaml_ng::from_str(yaml).context("parsing frontmatter")?;
    Ok((header, body.trim_start_matches('\n').trim_end().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrip() {
        let mut m = Message::new(
            Author { agent: "thermite/planner".into(), session: Some("s1".into()), swarm: None, harness: None },
            "backend",
            "Hello **world**\n\nSecond paragraph.",
        );
        m.kind = Kind::Finding;
        m.priority = Priority::High;
        m.to = vec!["thermite/reviewer".into()];
        m.refs = vec![Ref { repo: Some("org/repo".into()), commit: Some("abc".into()), ..Default::default() }];
        let text = m.to_markdown().unwrap();
        assert!(text.starts_with("---\n"));
        let back = Message::from_markdown(&text).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.body, m.body);
        assert_eq!(back.kind, Kind::Finding);
        assert_eq!(back.priority, Priority::High);
        assert_eq!(back.to, m.to);
        assert_eq!(back.refs, m.refs);
    }

    #[test]
    fn presence_roundtrip() {
        let p = Presence {
            agent: "thermite/planner".into(),
            session: Some("abc".into()),
            role: Some("planner".into()),
            swarm: Some("backend".into()),
            harness: Some("claude-code".into()),
            status: "active".into(),
            focus: None,
            last_seen: Timestamp::now(),
            projects: vec!["org/repo".into()],
            subscriptions: vec!["general".into()],
            notes: String::new(),
        };
        let back = Presence::from_markdown(&p.to_markdown().unwrap()).unwrap();
        assert_eq!(back.agent, p.agent);
        assert_eq!(back.subscriptions, p.subscriptions);
    }

    #[test]
    fn priority_orders() {
        assert!(Priority::Urgent > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
    }
}
