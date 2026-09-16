//! Thin wrapper over the `git` CLI. Uses the operator's existing credentials.
//!
//! All functions are blocking; call from `spawn_blocking` in async code.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Repo {
    pub path: PathBuf,
    pub remote: String,
}

const REALM_README: &str = "# Waystation realm\n\nThis repository is a Waystation coordination realm. \
Messages live under `channels/`, presence under `agents/`. \
Files are Markdown with YAML frontmatter; you can post by adding a file through the GitHub UI.\n";

const REALM_TOML: &str = "# Waystation realm configuration\n[realm]\nschema = 1\n\n[channels.general]\nretention_days = 90\n";

impl Repo {
    /// Open an existing clone or clone the remote into `path`.
    pub fn open_or_clone(remote: &str, path: &Path) -> Result<Self> {
        let repo = Self { path: path.to_path_buf(), remote: remote.to_string() };
        if path.join(".git").exists() {
            match repo.git(&["remote", "get-url", "origin"]) {
                Ok(url) if url == remote => {
                    repo.recover();
                    return Ok(repo);
                }
                other => {
                    tracing::info!(path = %path.display(), "clone points at {:?}, expected {remote}; re-cloning", other.ok());
                    std::fs::remove_dir_all(path)?;
                }
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = Command::new("git")
            .args(["clone", "--quiet", remote])
            .arg(path)
            .output()
            .context("running git clone")?;
        if !out.status.success() {
            bail!("git clone failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(repo)
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(args)
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    }

    fn git_ok(&self, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn has_commits(&self) -> bool {
        self.git_ok(&["rev-parse", "--verify", "-q", "HEAD"])
    }

    /// Local branch name (`main` for a fresh empty clone).
    pub fn branch(&self) -> String {
        self.git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .ok()
            .filter(|b| b != "HEAD")
            .unwrap_or_else(|| "main".into())
    }

    /// Create the realm layout and first commit if the repo is empty, and push it.
    pub fn ensure_initialized(&self) -> Result<bool> {
        if self.has_commits() {
            return Ok(false);
        }
        self.git(&["checkout", "-q", "-B", "main"]).ok();
        std::fs::write(self.path.join("README.md"), REALM_README)?;
        std::fs::write(self.path.join("waystation.toml"), REALM_TOML)?;
        std::fs::create_dir_all(self.path.join("channels/general"))?;
        std::fs::write(self.path.join("channels/general/.keep"), "")?;
        std::fs::create_dir_all(self.path.join("agents"))?;
        std::fs::write(self.path.join("agents/.keep"), "")?;
        self.git(&["add", "-A"])?;
        self.git(&["commit", "-q", "-m", "Initialize Waystation realm"])?;
        self.git(&["push", "-u", "origin", "HEAD"])?;
        Ok(true)
    }

    /// Undo the traces of a process that died mid-operation.
    pub fn recover(&self) {
        let git_dir = self.path.join(".git");
        if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
            tracing::warn!(path = %self.path.display(), "aborting interrupted rebase");
            let _ = self.git(&["rebase", "--abort"]);
        }
        if git_dir.join("index.lock").exists() {
            tracing::warn!(path = %self.path.display(), "removing stale index.lock");
            let _ = std::fs::remove_file(git_dir.join("index.lock"));
        }
    }

    pub fn fetch(&self) -> Result<()> {
        self.git(&["fetch", "--quiet", "origin"])?;
        Ok(())
    }

    /// Ask the remote for a single ref without fetching anything.
    pub fn ls_remote_head(&self, branch: &str) -> Result<Option<String>> {
        let out = self.git(&["ls-remote", "--quiet", "origin", &format!("refs/heads/{branch}")])?;
        Ok(out.split_whitespace().next().map(str::to_string))
    }

    pub fn local_head(&self) -> Result<String> {
        self.git(&["rev-parse", "HEAD"])
    }

    /// Commit id of `origin/<branch>`, or None if the remote is empty.
    pub fn remote_head(&self) -> Result<Option<String>> {
        let branch = self.branch();
        let r = format!("origin/{branch}");
        if self.git_ok(&["rev-parse", "--verify", "-q", &r]) {
            Ok(Some(self.git(&["rev-parse", &r])?))
        } else {
            Ok(None)
        }
    }

    /// Paths of files added between two commits (or all files at `to` when `from` is None).
    pub fn added_files(&self, from: Option<&str>, to: &str) -> Result<Vec<String>> {
        let out = match from {
            Some(f) if f != to => {
                self.git(&["diff", "--name-only", "--diff-filter=A", &format!("{f}..{to}")])?
            }
            Some(_) => return Ok(Vec::new()),
            None => self.git(&["ls-tree", "-r", "--name-only", to])?,
        };
        Ok(out.lines().filter(|l| !l.is_empty()).map(String::from).collect())
    }

    pub fn read_at(&self, rev: &str, path: &str) -> Result<String> {
        self.git(&["show", &format!("{rev}:{path}")])
    }

    /// Bring the working tree up to the remote: fast-forward, or rebase local commits on top.
    pub fn sync_worktree(&self) -> Result<()> {
        let branch = self.branch();
        let r = format!("origin/{branch}");
        if !self.git_ok(&["rev-parse", "--verify", "-q", &r]) {
            return Ok(());
        }
        if self.git_ok(&["merge", "--ff-only", "-q", &r]) {
            return Ok(());
        }
        self.git(&["rebase", "-q", &r])
            .map(|_| ())
            .context("rebasing local commits onto remote")
    }

    /// Write files, commit, and push (fetch + rebase + retry on rejection).
    pub fn commit_and_push(&self, files: &[(String, String)], message: &str) -> Result<String> {
        for (rel, content) in files {
            let full = self.path.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, content)?;
            self.git(&["add", "--", rel])?;
        }
        self.git(&["commit", "-q", "-m", message])?;
        self.push_with_retry(5)?;
        // After a rebase-and-retry the commit id differs from the one first created.
        self.local_head()
    }

    pub fn push_with_retry(&self, attempts: u32) -> Result<()> {
        let branch = self.branch();
        let mut last = None;
        for i in 0..attempts {
            match self.git(&["push", "--quiet", "origin", &format!("HEAD:{branch}")]) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    self.fetch()?;
                    self.sync_worktree()?;
                    let backoff = 200u64 * (1 << i) + (std::process::id() as u64 % 150);
                    std::thread::sleep(std::time::Duration::from_millis(backoff));
                }
            }
        }
        Err(last.unwrap()).context("push rejected after retries")
    }

    /// Read a file from the working tree.
    pub fn read_worktree(&self, rel: &str) -> Result<String> {
        std::fs::read_to_string(self.path.join(rel))
            .with_context(|| format!("reading {rel}"))
    }

    /// Recursively list files under a working-tree directory (relative paths).
    pub fn list_worktree(&self, rel_dir: &str) -> Result<Vec<String>> {
        let root = self.path.join(rel_dir);
        let mut out = Vec::new();
        if !root.exists() {
            return Ok(out);
        }
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(rel) = p.strip_prefix(&self.path) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        out.sort();
        Ok(out)
    }
}
