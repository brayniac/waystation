//! Per-machine daemon: one clone per realm, one poller, many session clients.
//!
//! Protocol: JSON lines over a Unix socket. Requests are
//! `{"id": n, "op": "...", "params": {...}}`, answered by
//! `{"id": n, "ok": true, "result": ...}` or `{"id": n, "ok": false, "error": "..."}`.
//! The daemon also sends `{"notify": "head", "realm": "...", "head": "..."}` whenever
//! a realm's remote head moves, and `{"notify": "shutdown"}` before exiting.

use crate::backend::{Backend, LocalBackend, socket_path};
use crate::config::Config;
use crate::model::{Message, Presence};
use crate::poller;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use ulid::Ulid;

/// Exit after this long with no connected clients.
const IDLE_EXIT: Duration = Duration::from_secs(300);

#[allow(clippy::readonly_write_lock)]
struct Shared {
    backend: RwLock<LocalBackend>,
    /// realm → last known remote head
    heads: Mutex<BTreeMap<String, String>>,
    clients: Mutex<Vec<Arc<Mutex<UnixStream>>>>,
    client_count: AtomicUsize,
    stop: AtomicBool,
    poll_now: (Mutex<bool>, std::sync::Condvar),
    config: Config,
}

impl Shared {
    fn broadcast(&self, msg: &Value) {
        let line = format!("{msg}\n");
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|c| {
            let mut s = c.lock().unwrap();
            s.write_all(line.as_bytes()).and_then(|_| s.flush()).is_ok()
        });
    }

    /// Check every realm's remote; fetch and notify when a head moved.
    #[allow(clippy::readonly_write_lock)]
    fn poll_once(&self) {
        let realms: Vec<(String, String)> = {
            let b = self.backend.read().unwrap();
            b.stores.values().map(|s| (s.realm.clone(), s.repo.branch())).collect()
        };
        for (realm, branch) in realms {
            let remote_sha = {
                let b = self.backend.read().unwrap();
                match b.stores[&realm].repo.ls_remote_head(&branch) {
                    Ok(Some(sha)) => sha,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(%realm, "ls-remote failed: {e:#}");
                        continue;
                    }
                }
            };
            let known = self.heads.lock().unwrap().get(&realm).cloned();
            if known.as_deref() == Some(remote_sha.as_str()) {
                continue;
            }
            {
                let b = self.backend.write().unwrap();
                let repo = &b.stores[&realm].repo;
                if let Err(e) = repo.fetch().and_then(|_| repo.sync_worktree()) {
                    tracing::warn!(%realm, "fetch failed: {e:#}");
                    continue;
                }
            }
            self.heads.lock().unwrap().insert(realm.clone(), remote_sha.clone());
            tracing::info!(%realm, head = &remote_sha[..8], "head moved");
            self.broadcast(&json!({ "notify": "head", "realm": realm, "head": remote_sha }));
        }
    }
}

pub fn run(config: Config) -> Result<()> {
    config.validate()?;
    let path = socket_path();
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            bail!("a daemon is already listening at {}", path.display());
        }
        std::fs::remove_file(&path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let backend = LocalBackend::open(&config, "daemon")?;
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    tracing::info!(socket = %path.display(), realms = backend.stores.len(), "daemon listening");

    let shared = Arc::new(Shared {
        backend: RwLock::new(backend),
        heads: Mutex::new(BTreeMap::new()),
        clients: Mutex::new(Vec::new()),
        client_count: AtomicUsize::new(0),
        stop: AtomicBool::new(false),
        poll_now: (Mutex::new(false), std::sync::Condvar::new()),
        config: config.clone(),
    });
    // Seed heads from the clones so the first poll only reports real movement.
    {
        let b = shared.backend.read().unwrap();
        let mut heads = shared.heads.lock().unwrap();
        for s in b.stores.values() {
            if let Ok(Some(h)) = s.repo.remote_head() {
                heads.insert(s.realm.clone(), h);
            }
        }
    }

    // Poller thread.
    {
        let shared = shared.clone();
        std::thread::Builder::new().name("poller".into()).spawn(move || {
            let mut idle_since: Option<Instant> = None;
            loop {
                if shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                shared.poll_once();
                let n = shared.client_count.load(Ordering::SeqCst);
                if n == 0 {
                    let since = *idle_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > IDLE_EXIT {
                        tracing::info!("no clients for {IDLE_EXIT:?}; exiting");
                        shared.stop.store(true, Ordering::SeqCst);
                        let _ = UnixStream::connect(socket_path()); // unblock accept()
                        return;
                    }
                } else {
                    idle_since = None;
                }
                let interval = Duration::from_secs(shared.config.poll.interval_secs);
                let (flag, cv) = &shared.poll_now;
                let mut f = flag.lock().unwrap();
                if !*f {
                    let (g, _) = cv.wait_timeout(f, interval).unwrap();
                    f = g;
                }
                *f = false;
            }
        })?;
    }

    // Accept loop.
    for conn in listener.incoming() {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = conn else { continue };
        let shared = shared.clone();
        std::thread::spawn(move || serve_client(shared, stream));
    }
    shared.broadcast(&json!({ "notify": "shutdown" }));
    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn serve_client(shared: Arc<Shared>, stream: UnixStream) {
    let writer = Arc::new(Mutex::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    }));
    shared.clients.lock().unwrap().push(writer.clone());
    shared.client_count.fetch_add(1, Ordering::SeqCst);
    let mut lines = BufReader::new(stream).lines();
    while let Some(Ok(line)) = lines.next() {
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let op = req.get("op").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let resp = match handle(&shared, op, params) {
            Ok(result) => json!({ "id": id, "ok": true, "result": result }),
            Err(e) => json!({ "id": id, "ok": false, "error": format!("{e:#}") }),
        };
        let mut w = writer.lock().unwrap();
        if w.write_all(format!("{resp}\n").as_bytes()).and_then(|_| w.flush()).is_err() {
            break;
        }
        if op == "shutdown" {
            shared.stop.store(true, Ordering::SeqCst);
            let (flag, cv) = &shared.poll_now;
            *flag.lock().unwrap() = true;
            cv.notify_all();
            let _ = UnixStream::connect(socket_path());
            break;
        }
    }
    shared.client_count.fetch_sub(1, Ordering::SeqCst);
    shared.clients.lock().unwrap().retain(|c| !Arc::ptr_eq(c, &writer));
}

fn str_param<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| anyhow!("missing `{key}`"))
}

#[allow(clippy::readonly_write_lock)]
fn handle(shared: &Shared, op: &str, p: Value) -> Result<Value> {
    match op {
        "hello" => {
            let b = shared.backend.read().unwrap();
            Ok(json!({ "version": env!("CARGO_PKG_VERSION"), "realms": b.realms() }))
        }
        "events_since" => {
            let realm = str_param(&p, "realm")?;
            let cursor = p.get("cursor").and_then(Value::as_str);
            let head = shared.heads.lock().unwrap().get(realm).cloned();
            let b = shared.backend.read().unwrap();
            let store = b.stores.get(realm).ok_or_else(|| anyhow!("unknown realm `{realm}`"))?;
            let Some(head) = head.or(store.repo.remote_head()?) else {
                return Ok(serde_json::to_value(poller::Tick::default())?);
            };
            let tick = poller::diff(store, cursor, &head)?;
            Ok(serde_json::to_value(tick)?)
        }
        "post" => {
            let realm = str_param(&p, "realm")?;
            let msg: Message = serde_json::from_value(p.get("message").cloned().unwrap_or(Value::Null))?;
            let commit = {
                let b = shared.backend.write().unwrap();
                b.post(realm, &msg)?
            };
            shared.heads.lock().unwrap().insert(realm.to_string(), commit.clone());
            shared.broadcast(&json!({ "notify": "head", "realm": realm, "head": commit }));
            Ok(json!(commit))
        }
        "presence" => {
            let realm = str_param(&p, "realm")?;
            let pr: Presence = serde_json::from_value(p.get("presence").cloned().unwrap_or(Value::Null))?;
            let commit = {
                let b = shared.backend.write().unwrap();
                b.write_presence(realm, &pr)?
            };
            shared.heads.lock().unwrap().insert(realm.to_string(), commit.clone());
            Ok(json!(commit))
        }
        "list_messages" => {
            let realm = str_param(&p, "realm")?;
            let channel = p.get("channel").and_then(Value::as_str);
            let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let b = shared.backend.read().unwrap();
            Ok(serde_json::to_value(b.list_messages(realm, channel, limit)?)?)
        }
        "find_message" => {
            let realm = str_param(&p, "realm")?;
            let id = Ulid::from_string(str_param(&p, "id")?)?;
            let b = shared.backend.read().unwrap();
            Ok(serde_json::to_value(b.find_message(realm, id)?)?)
        }
        "list_agents" => {
            let realm = str_param(&p, "realm")?;
            let b = shared.backend.read().unwrap();
            Ok(serde_json::to_value(b.list_agents(realm)?)?)
        }
        "sync" => {
            shared.poll_once();
            Ok(Value::Null)
        }
        "status" => {
            let heads = shared.heads.lock().unwrap().clone();
            Ok(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
                "clients": shared.client_count.load(Ordering::SeqCst),
                "heads": heads,
            }))
        }
        "shutdown" => Ok(Value::Null),
        other => bail!("unknown op `{other}`"),
    }
}

/// Client-side helpers for `waystation daemon status|stop`.
pub fn control(op: &str) -> Result<Value> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("no daemon at {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(format!("{}\n", json!({ "id": 1, "op": op, "params": {} })).as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let v: Value = serde_json::from_str(&line)?;
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    } else {
        bail!("{}", v.get("error").and_then(Value::as_str).unwrap_or("error"))
    }
}
