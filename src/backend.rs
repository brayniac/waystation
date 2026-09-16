//! Storage backends: a local clone per process, or a client of the per-machine daemon.
//!
//! `Core` talks only to the `Backend` trait, so the inbox, MCP facade, and CLI are
//! identical whether or not a daemon is running.

use crate::config::{Config, Trust, home_dir};
use crate::model::{Message, Presence};
use crate::poller::{self, Tick};
use crate::repo::Repo;
use crate::store::Store;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;
use ulid::Ulid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealmInfo {
    pub name: String,
    pub trust: Trust,
    pub agent_id: String,
    pub subscriptions: Vec<String>,
    pub remote: String,
}

pub trait Backend: Send + Sync {
    fn realms(&self) -> Vec<RealmInfo>;
    /// Events added after `cursor` (or none, pinning at head, when `cursor` is None).
    fn events_since(&self, realm: &str, cursor: Option<&str>) -> Result<Tick>;
    fn post(&self, realm: &str, msg: &Message) -> Result<String>;
    fn write_presence(&self, realm: &str, p: &Presence) -> Result<String>;
    fn list_messages(&self, realm: &str, channel: Option<&str>, limit: usize) -> Result<Vec<Message>>;
    fn find_message(&self, realm: &str, id: Ulid) -> Result<Option<Message>>;
    fn list_agents(&self, realm: &str) -> Result<Vec<Presence>>;
    /// Check the remotes right now.
    fn sync(&self) -> Result<()>;
    /// Block until the backend signals new data or `timeout` passes. Returns true on a signal.
    fn wait_nudge(&self, timeout: Duration) -> bool;
    fn describe(&self) -> String;
}

// ---------------------------------------------------------------------------
// Local: one clone per realm inside this process.

pub struct LocalBackend {
    pub stores: BTreeMap<String, Store>,
}

impl LocalBackend {
    pub fn open(config: &Config, clone_session: &str) -> Result<Self> {
        let mut stores = BTreeMap::new();
        for (name, rc) in &config.realm {
            let dir = config.clone_dir(name, clone_session)?;
            config.gc_stale_clones(&dir, Duration::from_secs(7 * 24 * 3600));
            let repo = Repo::open_or_clone(&rc.remote, &dir)
                .with_context(|| format!("opening realm `{name}`"))?;
            stores.insert(
                name.clone(),
                Store {
                    realm: name.clone(),
                    trust: rc.trust,
                    agent_id: config.agent_id(name)?,
                    subscriptions: rc.subscribe.clone(),
                    repo,
                },
            );
        }
        Ok(Self { stores })
    }

    fn store(&self, realm: &str) -> Result<&Store> {
        self.stores.get(realm).with_context(|| format!("realm `{realm}` is not mounted"))
    }
}

impl Backend for LocalBackend {
    fn realms(&self) -> Vec<RealmInfo> {
        self.stores
            .values()
            .map(|s| RealmInfo {
                name: s.realm.clone(),
                trust: s.trust,
                agent_id: s.agent_id.clone(),
                subscriptions: s.subscriptions.clone(),
                remote: s.repo.remote.clone(),
            })
            .collect()
    }
    fn events_since(&self, realm: &str, cursor: Option<&str>) -> Result<Tick> {
        poller::tick(self.store(realm)?, cursor)
    }
    fn post(&self, realm: &str, msg: &Message) -> Result<String> {
        self.store(realm)?.post(msg)
    }
    fn write_presence(&self, realm: &str, p: &Presence) -> Result<String> {
        self.store(realm)?.write_presence(p)
    }
    fn list_messages(&self, realm: &str, channel: Option<&str>, limit: usize) -> Result<Vec<Message>> {
        self.store(realm)?.list_messages(channel, limit)
    }
    fn find_message(&self, realm: &str, id: Ulid) -> Result<Option<Message>> {
        self.store(realm)?.find_message(id)
    }
    fn list_agents(&self, realm: &str) -> Result<Vec<Presence>> {
        self.store(realm)?.list_agents()
    }
    fn sync(&self) -> Result<()> {
        for s in self.stores.values() {
            s.repo.fetch()?;
            s.repo.sync_worktree()?;
        }
        Ok(())
    }
    fn wait_nudge(&self, timeout: Duration) -> bool {
        std::thread::sleep(timeout);
        false
    }
    fn describe(&self) -> String {
        "standalone (own clone per realm)".into()
    }
}

// ---------------------------------------------------------------------------
// Remote: client of `waystation daemon` over a Unix socket, JSON lines.

pub fn socket_path() -> PathBuf {
    home_dir().join("daemon.sock")
}

struct Conn {
    writer: UnixStream,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>,
    closed: Arc<AtomicBool>,
}

pub struct RemoteBackend {
    conn: Mutex<Option<Conn>>,
    next_id: AtomicU64,
    nudge: Arc<(Mutex<bool>, Condvar)>,
    realms: Mutex<Vec<RealmInfo>>,
    session: String,
    harness: Option<String>,
    path: PathBuf,
}

impl RemoteBackend {
    pub fn connect(session: &str, harness: Option<&str>) -> Result<Self> {
        let me = Self {
            conn: Mutex::new(None),
            next_id: AtomicU64::new(1),
            nudge: Arc::new((Mutex::new(false), Condvar::new())),
            realms: Mutex::new(Vec::new()),
            session: session.to_string(),
            harness: harness.map(str::to_string),
            path: socket_path(),
        };
        me.reconnect()?;
        Ok(me)
    }

    /// Open a fresh socket, start the reader thread, and redo the hello handshake.
    fn reconnect(&self) -> Result<()> {
        let stream = UnixStream::connect(&self.path)
            .with_context(|| format!("connecting to daemon at {}", self.path.display()))?;
        let reader = stream.try_clone()?;
        let pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>> = Default::default();
        let closed = Arc::new(AtomicBool::new(false));
        {
            let pending = pending.clone();
            let nudge = self.nudge.clone();
            let closed = closed.clone();
            std::thread::Builder::new().name("daemon-reader".into()).spawn(move || {
                let mut lines = BufReader::new(reader).lines();
                while let Some(Ok(line)) = lines.next() {
                    let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                    if let Some(id) = v.get("id").and_then(Value::as_u64) {
                        if let Some(tx) = pending.lock().unwrap().remove(&id) {
                            let _ = tx.send(v);
                        }
                    } else if v.get("notify").is_some() {
                        let (flag, cv) = &*nudge;
                        *flag.lock().unwrap() = true;
                        cv.notify_all();
                    }
                }
                closed.store(true, Ordering::SeqCst);
                pending.lock().unwrap().clear();
                // Wake any waiter so the next drain notices and reconnects.
                let (flag, cv) = &*nudge;
                *flag.lock().unwrap() = true;
                cv.notify_all();
            })?;
        }
        *self.conn.lock().unwrap() = Some(Conn { writer: stream, pending, closed });
        let hello = self.send("hello", json!({ "session": self.session, "harness": self.harness }), Duration::from_secs(10))?;
        *self.realms.lock().unwrap() = serde_json::from_value(hello.get("realms").cloned().unwrap_or(Value::Null))
            .context("bad hello response from daemon")?;
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.conn.lock().unwrap().as_ref().is_none_or(|c| c.closed.load(Ordering::SeqCst))
    }

    /// Reconnect if the daemon went away, starting a new one when necessary.
    fn ensure(&self) -> Result<()> {
        if !self.is_closed() {
            return Ok(());
        }
        tracing::warn!("daemon connection lost; reconnecting");
        if self.reconnect().is_ok() {
            return Ok(());
        }
        spawn_daemon()?;
        let mut last = None;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(250));
            match self.reconnect() {
                Ok(()) => {
                    tracing::info!("reconnected to a new daemon");
                    return Ok(());
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("daemon did not come back"))).context("reconnecting to daemon")
    }

    /// One request on the current connection (no reconnect logic).
    fn send(&self, op: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        let line = json!({ "id": id, "op": op, "params": params }).to_string();
        {
            let mut guard = self.conn.lock().unwrap();
            let conn = guard.as_mut().ok_or_else(|| anyhow!("not connected to daemon"))?;
            if conn.closed.load(Ordering::SeqCst) {
                bail!("daemon connection closed");
            }
            conn.pending.lock().unwrap().insert(id, tx);
            conn.writer.write_all(line.as_bytes())?;
            conn.writer.write_all(b"\n")?;
            conn.writer.flush()?;
        }
        let resp = rx
            .recv_timeout(timeout)
            .map_err(|_| anyhow!("daemon did not answer `{op}` within {timeout:?}"))?;
        if resp.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(resp.get("result").cloned().unwrap_or(Value::Null))
        } else {
            let msg = resp.get("error").and_then(Value::as_str).unwrap_or("unknown daemon error");
            Err(anyhow!("{msg}"))
        }
    }

    fn request(&self, op: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.ensure()?;
        match self.send(op, params.clone(), timeout) {
            Ok(v) => Ok(v),
            Err(e) if self.is_closed() => {
                // The daemon vanished mid-request: reconnect once and retry.
                tracing::warn!("request `{op}` failed ({e:#}); retrying after reconnect");
                self.ensure()?;
                self.send(op, params, timeout)
            }
            Err(e) => Err(e),
        }
    }

    fn call<T: for<'de> Deserialize<'de>>(&self, op: &str, params: Value) -> Result<T> {
        let v = self.request(op, params, Duration::from_secs(180))?;
        serde_json::from_value(v).with_context(|| format!("decoding `{op}` response"))
    }
}

impl Backend for RemoteBackend {
    fn realms(&self) -> Vec<RealmInfo> {
        self.realms.lock().unwrap().clone()
    }
    fn events_since(&self, realm: &str, cursor: Option<&str>) -> Result<Tick> {
        self.call("events_since", json!({ "realm": realm, "cursor": cursor }))
    }
    fn post(&self, realm: &str, msg: &Message) -> Result<String> {
        self.call("post", json!({ "realm": realm, "message": msg }))
    }
    fn write_presence(&self, realm: &str, p: &Presence) -> Result<String> {
        self.call("presence", json!({ "realm": realm, "presence": p }))
    }
    fn list_messages(&self, realm: &str, channel: Option<&str>, limit: usize) -> Result<Vec<Message>> {
        self.call("list_messages", json!({ "realm": realm, "channel": channel, "limit": limit }))
    }
    fn find_message(&self, realm: &str, id: Ulid) -> Result<Option<Message>> {
        self.call("find_message", json!({ "realm": realm, "id": id.to_string() }))
    }
    fn list_agents(&self, realm: &str) -> Result<Vec<Presence>> {
        self.call("list_agents", json!({ "realm": realm }))
    }
    fn sync(&self) -> Result<()> {
        self.request("sync", json!({}), Duration::from_secs(60)).map(|_| ())
    }
    fn wait_nudge(&self, timeout: Duration) -> bool {
        let (flag, cv) = &*self.nudge;
        let mut f = flag.lock().unwrap();
        if !*f {
            let (g, _) = cv.wait_timeout(f, timeout).unwrap();
            f = g;
        }
        let fired = *f;
        *f = false;
        // A closed connection also wakes us; report it as a nudge so the caller
        // drains, which triggers the reconnect.
        fired
    }
    fn describe(&self) -> String {
        format!("daemon at {}", self.path.display())
    }
}

/// Connect to the daemon, starting one if needed. Returns None if that is impossible.
pub fn connect_or_start(session: &str, harness: Option<&str>) -> Option<RemoteBackend> {
    if let Ok(b) = RemoteBackend::connect(session, harness) {
        return Some(b);
    }
    if let Err(e) = spawn_daemon() {
        tracing::warn!("could not start daemon: {e:#}");
        return None;
    }
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(250));
        if let Ok(b) = RemoteBackend::connect(session, harness) {
            return Some(b);
        }
    }
    tracing::warn!("daemon did not come up in 10s; running standalone");
    None
}

fn spawn_daemon() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home_dir().join("daemon.log"))?;
    std::process::Command::new(exe)
        .arg("daemon")
        .arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()
        .context("spawning `waystation daemon run`")?;
    Ok(())
}
