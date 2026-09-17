# Trees and `waystation env` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve which realm tree a session belongs to from the repository it launches in, and print the environment for it.

**Architecture:** A `[tree]` table in each tree's `config.toml` names the tree and lists the projects it claims. The default tree at `~/.waystation` also lists sibling tree paths, so the roster is one hop from a fixed location. Resolution is a pure function from roster plus detected project to a tree; `waystation env` prints exports for the winner.

**Tech Stack:** Rust 2024, clap 4 (derive), serde + toml, anyhow, tempfile for tests. Tests are inline `#[cfg(test)] mod tests` modules, matching `src/core.rs:298` and `src/inbox.rs:208`.

**Spec:** `docs/superpowers/specs/2026-09-17-tree-launcher-design.md`

---

## File Structure

- `src/config.rs` — **modify.** Add the `TreeConfig` struct and a `tree` field on `Config`. Add `load_from`/`save_to` taking a tree directory, and `default_root()`. Existing `load`/`save` delegate to the `WAYSTATION_HOME` directory so every current caller is unchanged.
- `src/tree.rs` — **create.** The tree concept: `Tree`, `Roster`, claim matching, resolution, and the `env` output string. Filesystem access is confined to `Roster::load`; everything else is pure and table-testable.
- `src/main.rs` — **modify.** Declare `mod tree`, add a global hidden `--root`, add `Cmd::Env` and `Cmd::Tree`, add `setup --project`.
- `DESIGN.md`, `README.md` — **modify.** Document trees.

Resolution lives in `tree.rs` rather than `config.rs` because it reads *several* configs, while `config.rs` is about one. Keeping `Roster::load` as the only IO in the file is what lets the resolution table run without touching a filesystem.

---

### Task 1: The `[tree]` config table

**Files:**
- Modify: `src/config.rs:10-18` (the `Config` struct), after `src/config.rs:31` (new struct), `src/config.rs:135-157` (`load`/`save`), `src/config.rs:124-134` (`home_dir`/`config_path`)
- Test: `src/config.rs` (new inline `mod tests` at end of file)

- [ ] **Step 1: Write the failing test**

Append to the end of `src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_table_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.identity.operator = "brayniac".into();
        cfg.tree.name = Some("oss".into());
        cfg.tree.projects = vec!["brayniac/rezolus".into()];
        cfg.tree.siblings = vec![PathBuf::from("~/.waystation-work")];
        cfg.save_to(dir.path()).unwrap();

        let back = Config::load_from(dir.path()).unwrap();
        assert_eq!(back.tree.name.as_deref(), Some("oss"));
        assert_eq!(back.tree.projects, vec!["brayniac/rezolus".to_string()]);
        assert_eq!(back.tree.siblings, vec![PathBuf::from("~/.waystation-work")]);
    }

    #[test]
    fn missing_tree_table_defaults_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[identity]\noperator = \"x\"\n").unwrap();

        let cfg = Config::load_from(dir.path()).unwrap();
        assert_eq!(cfg.tree.name, None);
        assert!(cfg.tree.projects.is_empty());
        assert!(cfg.tree.siblings.is_empty());
    }

    #[test]
    fn load_from_missing_dir_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load_from(&dir.path().join("nope")).unwrap();
        assert!(cfg.realm.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib tree_table_round_trips`

Expected: FAIL to compile — `no field 'tree' on type 'Config'`, `no function or associated item named 'save_to'`.

- [ ] **Step 3: Write the implementation**

Add the struct after the `Identity` struct (after `src/config.rs:31`):

```rust
/// This tree: what it is called and which projects belong to it.
///
/// A tree is one `WAYSTATION_HOME`: one identity, one set of realms, one clone
/// set, one daemon. `siblings` is meaningful only in the default tree, which is
/// where the roster lives.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TreeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `owner/name` entries, as `detect_project` produces them, and `owner/*` globs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    /// Other trees on this machine. Read from the default tree only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub siblings: Vec<PathBuf>,
}
```

Add the field to `Config` (after the `poll` field, `src/config.rs:15-16`):

```rust
    #[serde(default)]
    pub tree: TreeConfig,
```

Add `default_root` beside `home_dir` (after `src/config.rs:130`):

```rust
/// Where tree resolution starts. Never `WAYSTATION_HOME`: an inherited value
/// would make a shell that once launched one tree keep resolving to it.
pub fn default_root() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".waystation")
}
```

Replace `load` and `save` (`src/config.rs:135-157`) with directory-taking versions plus delegating wrappers:

```rust
    pub fn load() -> Result<Self> {
        Self::load_from(&home_dir())
    }

    /// Load the config of the tree rooted at `dir`. A tree with no config file
    /// is a valid empty tree, not an error.
    pub fn load_from(dir: &Path) -> Result<Self> {
        let path = dir.join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&home_dir())
    }

    pub fn save_to(&self, dir: &Path) -> Result<()> {
        let path = dir.join("config.toml");
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib config::tests`

Expected: PASS, 3 tests.

- [ ] **Step 5: Check nothing else broke**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`

Expected: all existing tests pass, no warnings.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "config: add the [tree] table and directory-scoped load/save"
```

---

### Task 2: Claim matching

**Files:**
- Create: `src/tree.rs`
- Modify: `src/main.rs:1-10` (module list)
- Test: `src/tree.rs` (inline)

- [ ] **Step 1: Write the failing test**

Create `src/tree.rs` with only the test module and an empty function so the test compiles against a real signature:

```rust
//! Trees: one `WAYSTATION_HOME` each, selected by the project a session runs in.

/// Does `claim` cover `project`? Exact `owner/name`, or an `owner/*` glob.
pub fn claim_matches(claim: &str, project: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_match_exactly_or_by_owner_glob() {
        assert!(claim_matches("brayniac/rezolus", "brayniac/rezolus"));
        assert!(!claim_matches("brayniac/rezolus", "brayniac/llm-perf"));
        assert!(claim_matches("acme-corp/*", "acme-corp/api-gateway"));
        assert!(!claim_matches("acme-corp/*", "acme-corpse/api-gateway"));
        assert!(!claim_matches("acme-corp/*", "acme-corp"));
        // A bare owner is not a glob; claiming everything must be deliberate.
        assert!(!claim_matches("acme-corp", "acme-corp/api-gateway"));
        // Case follows the repository host: compare case-insensitively.
        assert!(claim_matches("Brayniac/Rezolus", "brayniac/rezolus"));
    }
}
```

Add to `src/main.rs` module list (after `mod store;`, `src/main.rs:10`):

```rust
mod tree;
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib claims_match_exactly_or_by_owner_glob`

Expected: FAIL — `assertion failed: claim_matches("brayniac/rezolus", "brayniac/rezolus")`.

- [ ] **Step 3: Write the implementation**

Replace the body of `claim_matches` in `src/tree.rs`:

```rust
pub fn claim_matches(claim: &str, project: &str) -> bool {
    let claim = claim.trim().to_lowercase();
    let project = project.trim().to_lowercase();
    if let Some(owner) = claim.strip_suffix("/*") {
        return project
            .split_once('/')
            .is_some_and(|(o, rest)| o == owner && !rest.is_empty());
    }
    claim == project
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib claims_match_exactly_or_by_owner_glob`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tree.rs src/main.rs
git commit -m "tree: claim matching, exact and owner glob"
```

---

### Task 3: Resolution

**Files:**
- Modify: `src/tree.rs`
- Test: `src/tree.rs` (inline)

- [ ] **Step 1: Write the failing test**

Add to `src/tree.rs`, above the existing `mod tests` content, the types the test needs — declaration only, so the test compiles and fails on behaviour:

```rust
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

/// One tree in the roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub name: String,
    pub path: PathBuf,
    pub projects: Vec<String>,
    /// The tree at `default_root()`: the fallback, and where the roster lives.
    pub is_default: bool,
}

/// Every tree known to this machine: the default tree and its siblings.
#[derive(Debug, Clone, Default)]
pub struct Roster {
    pub trees: Vec<Tree>,
    /// Roster entries that could not be read. Reported by `tree ls`, skipped otherwise.
    pub warnings: Vec<String>,
}

impl Roster {
    pub fn default_tree(&self) -> Result<&Tree> {
        self.trees.iter().find(|t| t.is_default).context("no default tree")
    }

    /// Pick a tree: a forced name wins, else the one claiming `project`, else the default.
    pub fn resolve(&self, project: Option<&str>, forced: Option<&str>) -> Result<&Tree> {
        let _ = (project, forced);
        bail!("not implemented")
    }
}
```

Replace the `mod tests` block in `src/tree.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, projects: &[&str], is_default: bool) -> Tree {
        Tree {
            name: name.into(),
            path: PathBuf::from(format!("/trees/{name}")),
            projects: projects.iter().map(|s| s.to_string()).collect(),
            is_default,
        }
    }

    fn roster() -> Roster {
        Roster {
            trees: vec![
                tree("default", &[], true),
                tree("work", &["acme-corp/*"], false),
                tree("oss", &["brayniac/rezolus", "brayniac/llm-perf"], false),
            ],
            warnings: vec![],
        }
    }

    #[test]
    fn claims_match_exactly_or_by_owner_glob() {
        assert!(claim_matches("brayniac/rezolus", "brayniac/rezolus"));
        assert!(!claim_matches("brayniac/rezolus", "brayniac/llm-perf"));
        assert!(claim_matches("acme-corp/*", "acme-corp/api-gateway"));
        assert!(!claim_matches("acme-corp/*", "acme-corpse/api-gateway"));
        assert!(!claim_matches("acme-corp/*", "acme-corp"));
        assert!(!claim_matches("acme-corp", "acme-corp/api-gateway"));
        assert!(claim_matches("Brayniac/Rezolus", "brayniac/rezolus"));
    }

    #[test]
    fn exact_claim_wins() {
        let r = roster();
        assert_eq!(r.resolve(Some("brayniac/rezolus"), None).unwrap().name, "oss");
    }

    #[test]
    fn glob_claim_wins() {
        let r = roster();
        assert_eq!(r.resolve(Some("acme-corp/api-gateway"), None).unwrap().name, "work");
    }

    #[test]
    fn unclaimed_project_falls_back_to_default() {
        let r = roster();
        assert_eq!(r.resolve(Some("someone/else"), None).unwrap().name, "default");
    }

    #[test]
    fn no_project_falls_back_to_default() {
        let r = roster();
        assert_eq!(r.resolve(None, None).unwrap().name, "default");
    }

    #[test]
    fn forced_tree_wins_over_claims() {
        let r = roster();
        assert_eq!(r.resolve(Some("brayniac/rezolus"), Some("work")).unwrap().name, "work");
    }

    #[test]
    fn unknown_forced_tree_lists_the_known_ones() {
        let r = roster();
        let err = r.resolve(None, Some("nope")).unwrap_err().to_string();
        assert!(err.contains("unknown tree `nope`"), "{err}");
        assert!(err.contains("default, work, oss"), "{err}");
    }

    #[test]
    fn two_claimants_is_an_error_naming_both() {
        let mut r = roster();
        r.trees.push(tree("second", &["brayniac/rezolus"], false));
        let err = r.resolve(Some("brayniac/rezolus"), None).unwrap_err().to_string();
        assert!(err.contains("brayniac/rezolus"), "{err}");
        assert!(err.contains("oss"), "{err}");
        assert!(err.contains("second"), "{err}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib tree::tests`

Expected: the resolution tests FAIL with `not implemented`; `claims_match_exactly_or_by_owner_glob` still passes.

- [ ] **Step 3: Write the implementation**

Replace `Roster::resolve` in `src/tree.rs`:

```rust
    pub fn resolve(&self, project: Option<&str>, forced: Option<&str>) -> Result<&Tree> {
        if let Some(name) = forced {
            return self.trees.iter().find(|t| t.name == name).with_context(|| {
                format!(
                    "unknown tree `{name}`; known trees: {}",
                    self.trees.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
                )
            });
        }
        let Some(project) = project else { return self.default_tree() };
        let claimants: Vec<&Tree> = self
            .trees
            .iter()
            .filter(|t| t.projects.iter().any(|c| claim_matches(c, project)))
            .collect();
        match claimants.as_slice() {
            [] => self.default_tree(),
            [one] => Ok(one),
            many => bail!(
                "project `{project}` is claimed by more than one tree ({}); \
                 remove the claim from all but one, or name the tree with --tree",
                many.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib tree::tests`

Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
git add src/tree.rs
git commit -m "tree: resolve a tree from a project, refusing ambiguous claims"
```

---

### Task 4: Loading the roster from disk

**Files:**
- Modify: `src/tree.rs`
- Test: `src/tree.rs` (inline)

- [ ] **Step 1: Write the failing test**

Add the declaration to `src/tree.rs` (inside `impl Roster`):

```rust
    /// Read the tree at `root` and every sibling it lists.
    pub fn load(root: &Path) -> Result<Roster> {
        let _ = root;
        bail!("not implemented")
    }
```

Add these free functions to `src/tree.rs`:

```rust
/// Expand a leading `~` against the home directory.
pub fn expand_tilde(p: &Path) -> PathBuf {
    let Ok(rest) = p.strip_prefix("~") else { return p.to_path_buf() };
    match dirs::home_dir() {
        Some(home) => home.join(rest),
        None => p.to_path_buf(),
    }
}

/// `~/.waystation` → `default`, `~/.waystation-work` → `work`, else the basename.
pub fn name_from_path(p: &Path) -> String {
    let base = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let base = base.strip_prefix('.').unwrap_or(&base).to_string();
    match base.strip_prefix("waystation-") {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ if base == "waystation" || base.is_empty() => "default".to_string(),
        _ => base,
    }
}
```

Add `use std::path::Path;` to the imports, and add these tests to `mod tests`:

```rust
    use crate::config::Config;

    fn write_tree(dir: &Path, name: Option<&str>, projects: &[&str], siblings: &[PathBuf]) {
        let mut cfg = Config::default();
        cfg.identity.operator = "brayniac".into();
        cfg.tree.name = name.map(|s| s.to_string());
        cfg.tree.projects = projects.iter().map(|s| s.to_string()).collect();
        cfg.tree.siblings = siblings.to_vec();
        cfg.save_to(dir).unwrap();
    }

    #[test]
    fn names_fall_back_to_the_directory() {
        assert_eq!(name_from_path(Path::new("/Users/x/.waystation")), "default");
        assert_eq!(name_from_path(Path::new("/Users/x/.waystation-work")), "work");
        assert_eq!(name_from_path(Path::new("/Users/x/realms")), "realms");
    }

    #[test]
    fn load_reads_the_default_tree_and_its_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        let work = tmp.path().join(".waystation-work");
        write_tree(&root, None, &[], &[work.clone()]);
        write_tree(&work, Some("work"), &["acme-corp/*"], &[]);

        let r = Roster::load(&root).unwrap();
        assert_eq!(r.trees.len(), 2);
        assert_eq!(r.default_tree().unwrap().name, "default");
        assert!(r.default_tree().unwrap().is_default);
        assert_eq!(r.resolve(Some("acme-corp/api"), None).unwrap().name, "work");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn a_missing_sibling_warns_and_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        let gone = tmp.path().join(".waystation-gone");
        write_tree(&root, None, &[], &[gone.clone()]);

        let r = Roster::load(&root).unwrap();
        assert_eq!(r.trees.len(), 1);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains(".waystation-gone"), "{:?}", r.warnings);
    }

    #[test]
    fn a_default_tree_with_no_config_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        let r = Roster::load(&root).unwrap();
        assert_eq!(r.trees.len(), 1);
        assert_eq!(r.resolve(None, None).unwrap().name, "default");
    }

    #[test]
    fn a_sibling_listed_twice_appears_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        let work = tmp.path().join(".waystation-work");
        write_tree(&root, None, &[], &[work.clone(), work.clone()]);
        write_tree(&work, Some("work"), &[], &[]);

        let r = Roster::load(&root).unwrap();
        assert_eq!(r.trees.len(), 2);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib tree::tests`

Expected: the four `load` tests FAIL with `not implemented`; `names_fall_back_to_the_directory` passes.

- [ ] **Step 3: Write the implementation**

Replace `Roster::load` in `src/tree.rs`:

```rust
    pub fn load(root: &Path) -> Result<Roster> {
        let root = expand_tilde(root);
        let cfg = Config::load_from(&root)?;
        let mut roster = Roster {
            trees: vec![Tree {
                name: cfg.tree.name.clone().unwrap_or_else(|| name_from_path(&root)),
                path: root.clone(),
                projects: cfg.tree.projects.clone(),
                is_default: true,
            }],
            warnings: vec![],
        };
        for sibling in &cfg.tree.siblings {
            let path = expand_tilde(sibling);
            if roster.trees.iter().any(|t| t.path == path) {
                continue;
            }
            if !path.join("config.toml").exists() {
                roster.warnings.push(format!(
                    "tree {} has no config.toml; skipped. Remove it with `waystation tree rm {}`",
                    path.display(),
                    sibling.display()
                ));
                continue;
            }
            let sc = Config::load_from(&path)?;
            roster.trees.push(Tree {
                name: sc.tree.name.clone().unwrap_or_else(|| name_from_path(&path)),
                path,
                projects: sc.tree.projects.clone(),
                is_default: false,
            });
        }
        Ok(roster)
    }
```

Add the import at the top of `src/tree.rs`:

```rust
use crate::config::Config;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib tree::tests`

Expected: PASS, 13 tests.

- [ ] **Step 5: Commit**

```bash
git add src/tree.rs
git commit -m "tree: load the roster from the default tree and its siblings"
```

---

### Task 5: `waystation env`

**Files:**
- Modify: `src/tree.rs` (add `env_exports`), `src/main.rs:38-115` (`Cmd`), `src/main.rs:20-35` (global `--root`), `src/main.rs:181` (dispatch)
- Test: `src/tree.rs` (inline)

- [ ] **Step 1: Write the failing test**

Add the declaration to `src/tree.rs`:

```rust
/// The shell lines `waystation env` prints for a resolved tree.
pub fn env_exports(tree: &Tree, project: Option<&str>) -> String {
    let _ = (tree, project);
    String::new()
}
```

Add to `mod tests`:

```rust
    #[test]
    fn exports_name_the_tree_and_the_project() {
        let t = tree("oss", &[], false);
        let out = env_exports(&t, Some("brayniac/rezolus"));
        assert_eq!(
            out,
            "export WAYSTATION_HOME=/trees/oss\nexport WAYSTATION_PROJECT=brayniac/rezolus\n"
        );
    }

    #[test]
    fn exports_omit_the_project_when_there_is_none() {
        let t = tree("default", &[], true);
        assert_eq!(env_exports(&t, None), "export WAYSTATION_HOME=/trees/default\n");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib exports_`

Expected: FAIL — `assertion \`left == right\` failed`, left is empty.

- [ ] **Step 3: Write the implementation**

Replace `env_exports` in `src/tree.rs`:

```rust
pub fn env_exports(tree: &Tree, project: Option<&str>) -> String {
    let mut out = format!("export WAYSTATION_HOME={}\n", tree.path.display());
    if let Some(p) = project {
        out.push_str(&format!("export WAYSTATION_PROJECT={p}\n"));
    }
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib exports_`

Expected: PASS, 2 tests.

- [ ] **Step 5: Add the global `--root` flag**

In `src/main.rs`, add to `struct Cli` after the `standalone` field (`src/main.rs:31-32`):

```rust
    /// Where tree resolution starts. Testing hook; defaults to `~/.waystation`.
    #[arg(long, global = true, hide = true)]
    root: Option<std::path::PathBuf>,
```

- [ ] **Step 6: Add the `Env` subcommand**

In `src/main.rs`, add to `enum Cmd` before `Serve` (`src/main.rs:105`):

```rust
    /// Resolve the tree for this directory and print shell exports.
    Env {
        /// Use this tree instead of the one claiming this repository.
        #[arg(long)]
        tree: Option<String>,
    },
```

- [ ] **Step 7: Dispatch it**

In `src/main.rs`, add before the `Cmd::Serve` arm:

```rust
        Cmd::Env { tree: forced } => {
            let root = cli.root.clone().unwrap_or_else(config::default_root);
            let roster = tree::Roster::load(&root)?;
            let project = core::detect_project();
            let chosen = roster.resolve(project.as_deref(), forced.as_deref())?;
            print!("{}", tree::env_exports(chosen, project.as_deref()));
            Ok(())
        }
```

- [ ] **Step 8: Verify the command end to end**

```bash
cargo build
TMP=$(mktemp -d)
mkdir -p "$TMP/.waystation" "$TMP/.waystation-oss"
printf '[identity]\noperator = "brayniac"\n\n[tree]\nsiblings = ["%s/.waystation-oss"]\n' "$TMP" > "$TMP/.waystation/config.toml"
printf '[identity]\noperator = "brayniac"\n\n[tree]\nname = "oss"\nprojects = ["brayniac/rezolus"]\n' > "$TMP/.waystation-oss/config.toml"
WAYSTATION_PROJECT=brayniac/rezolus ./target/debug/waystation env --root "$TMP/.waystation"
WAYSTATION_PROJECT=someone/else ./target/debug/waystation env --root "$TMP/.waystation"
```

Expected, in order:

```
export WAYSTATION_HOME=<TMP>/.waystation-oss
export WAYSTATION_PROJECT=brayniac/rezolus
export WAYSTATION_HOME=<TMP>/.waystation
export WAYSTATION_PROJECT=someone/else
```

- [ ] **Step 9: Commit**

```bash
git add src/tree.rs src/main.rs
git commit -m "cli: waystation env resolves a tree and prints its exports"
```

---

### Task 6: `waystation tree ls | add | rm`

**Files:**
- Modify: `src/main.rs` (`enum Cmd`, new `enum TreeCmd`, dispatch)
- Test: `src/tree.rs` (inline, for the roster mutations)

- [ ] **Step 1: Write the failing test**

Add the declarations to `src/tree.rs`:

```rust
/// Add `path` to the default tree's roster. Returns false if it was already there.
pub fn roster_add(root: &Path, path: &Path) -> Result<bool> {
    let _ = (root, path);
    bail!("not implemented")
}

/// Remove `path` from the default tree's roster. Returns false if it was absent.
pub fn roster_rm(root: &Path, path: &Path) -> Result<bool> {
    let _ = (root, path);
    bail!("not implemented")
}
```

Add to `mod tests`:

```rust
    #[test]
    fn roster_add_and_rm_edit_the_default_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        let work = tmp.path().join(".waystation-work");
        write_tree(&root, None, &[], &[]);
        write_tree(&work, Some("work"), &["acme-corp/*"], &[]);

        assert!(roster_add(&root, &work).unwrap());
        assert!(!roster_add(&root, &work).unwrap(), "adding twice is a no-op");
        assert_eq!(Roster::load(&root).unwrap().trees.len(), 2);

        assert!(roster_rm(&root, &work).unwrap());
        assert!(!roster_rm(&root, &work).unwrap(), "removing twice is a no-op");
        assert_eq!(Roster::load(&root).unwrap().trees.len(), 1);
    }

    #[test]
    fn roster_add_refuses_a_directory_that_is_not_a_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        write_tree(&root, None, &[], &[]);
        let err = roster_add(&root, &tmp.path().join("nothing-here")).unwrap_err().to_string();
        assert!(err.contains("no config.toml"), "{err}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib roster_`

Expected: FAIL with `not implemented`.

- [ ] **Step 3: Write the implementation**

Replace the two functions in `src/tree.rs`:

```rust
pub fn roster_add(root: &Path, path: &Path) -> Result<bool> {
    let root = expand_tilde(root);
    let path = expand_tilde(path);
    if !path.join("config.toml").exists() {
        bail!(
            "{} has no config.toml; set the tree up first with \
             `WAYSTATION_HOME={} waystation setup ...`",
            path.display(),
            path.display()
        );
    }
    if path == root {
        bail!("{} is the default tree; it is always in the roster", path.display());
    }
    let mut cfg = Config::load_from(&root)?;
    if cfg.tree.siblings.iter().any(|s| expand_tilde(s) == path) {
        return Ok(false);
    }
    cfg.tree.siblings.push(path);
    cfg.save_to(&root)?;
    Ok(true)
}

pub fn roster_rm(root: &Path, path: &Path) -> Result<bool> {
    let root = expand_tilde(root);
    let path = expand_tilde(path);
    let mut cfg = Config::load_from(&root)?;
    let before = cfg.tree.siblings.len();
    cfg.tree.siblings.retain(|s| expand_tilde(s) != path);
    if cfg.tree.siblings.len() == before {
        return Ok(false);
    }
    cfg.save_to(&root)?;
    Ok(true)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib roster_`

Expected: PASS, 2 tests.

- [ ] **Step 5: Add the subcommands**

In `src/main.rs`, add to `enum Cmd` after the `Env` arm:

```rust
    /// Inspect and maintain the trees on this machine.
    Tree {
        #[command(subcommand)]
        cmd: TreeCmd,
    },
```

Add the enum next to `RealmCmd` (`src/main.rs:125-131`):

```rust
#[derive(Subcommand)]
enum TreeCmd {
    /// List the trees, what they claim, and which one wins here.
    Ls,
    /// Add a tree to the roster.
    Add { path: std::path::PathBuf },
    /// Remove a tree from the roster. The tree's own directory is left alone.
    Rm { path: std::path::PathBuf },
}
```

- [ ] **Step 6: Dispatch them**

In `src/main.rs`, add after the `Cmd::Env` arm:

```rust
        Cmd::Tree { cmd } => {
            let root = cli.root.clone().unwrap_or_else(config::default_root);
            match cmd {
                TreeCmd::Ls => {
                    let roster = tree::Roster::load(&root)?;
                    let project = core::detect_project();
                    let chosen = roster.resolve(project.as_deref(), None).ok().map(|t| t.name.clone());
                    for t in &roster.trees {
                        let marker = if Some(&t.name) == chosen.as_ref() { " <- here" } else { "" };
                        println!(
                            "{}\t{}\t{}\tprojects={}{}",
                            t.name,
                            t.path.display(),
                            if t.is_default { "default" } else { "-" },
                            if t.projects.is_empty() { "-".into() } else { t.projects.join(",") },
                            marker
                        );
                    }
                    for w in &roster.warnings {
                        eprintln!("warning: {w}");
                    }
                    Ok(())
                }
                TreeCmd::Add { path } => {
                    if tree::roster_add(&root, &path)? {
                        println!("added {}", path.display());
                    } else {
                        println!("{} is already in the roster", path.display());
                    }
                    Ok(())
                }
                TreeCmd::Rm { path } => {
                    if tree::roster_rm(&root, &path)? {
                        println!("removed {}", path.display());
                    } else {
                        println!("{} is not in the roster", path.display());
                    }
                    Ok(())
                }
            }
        }
```

- [ ] **Step 7: Verify end to end**

```bash
cargo build
TMP=$(mktemp -d)
mkdir -p "$TMP/.waystation" "$TMP/.waystation-work"
printf '[identity]\noperator = "brayniac"\n' > "$TMP/.waystation/config.toml"
printf '[identity]\noperator = "brayniac"\n\n[tree]\nname = "work"\nprojects = ["acme-corp/*"]\n' > "$TMP/.waystation-work/config.toml"
./target/debug/waystation tree add "$TMP/.waystation-work" --root "$TMP/.waystation"
WAYSTATION_PROJECT=acme-corp/api ./target/debug/waystation tree ls --root "$TMP/.waystation"
./target/debug/waystation tree rm "$TMP/.waystation-work" --root "$TMP/.waystation"
```

Expected:

```
added <TMP>/.waystation-work
default	<TMP>/.waystation	default	projects=-
work	<TMP>/.waystation-work	-	projects=acme-corp/* <- here
removed <TMP>/.waystation-work
```

- [ ] **Step 8: Commit**

```bash
git add src/tree.rs src/main.rs
git commit -m "cli: waystation tree ls/add/rm"
```

---

### Task 7: `setup --project`

**Files:**
- Modify: `src/main.rs:39-56` (`Cmd::Setup` fields), `src/main.rs:182-216` (the `Setup` arm)
- Test: `src/tree.rs` (inline, for the claim helper)

- [ ] **Step 1: Write the failing test**

Add the declaration to `src/tree.rs`:

```rust
/// Add a project claim to the tree rooted at `dir`. Returns false if already claimed.
pub fn claim_project(dir: &Path, project: &str) -> Result<bool> {
    let _ = (dir, project);
    bail!("not implemented")
}
```

Add to `mod tests`:

```rust
    #[test]
    fn claiming_a_project_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".waystation-oss");
        write_tree(&dir, Some("oss"), &[], &[]);

        assert!(claim_project(&dir, "brayniac/rezolus").unwrap());
        assert!(!claim_project(&dir, "brayniac/rezolus").unwrap());
        assert!(claim_project(&dir, "brayniac/llm-perf").unwrap());

        let cfg = Config::load_from(&dir).unwrap();
        assert_eq!(cfg.tree.projects, vec!["brayniac/rezolus".to_string(), "brayniac/llm-perf".to_string()]);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib claiming_a_project_is_idempotent`

Expected: FAIL with `not implemented`.

- [ ] **Step 3: Write the implementation**

Replace `claim_project` in `src/tree.rs`:

```rust
pub fn claim_project(dir: &Path, project: &str) -> Result<bool> {
    let dir = expand_tilde(dir);
    let mut cfg = Config::load_from(&dir)?;
    if cfg.tree.projects.iter().any(|c| c.eq_ignore_ascii_case(project)) {
        return Ok(false);
    }
    cfg.tree.projects.push(project.to_string());
    cfg.save_to(&dir)?;
    Ok(true)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib claiming_a_project_is_idempotent`

Expected: PASS.

- [ ] **Step 5: Wire it into `setup`**

In `src/main.rs`, add to the `Cmd::Setup` fields after `swarm` (`src/main.rs:45-46`):

```rust
        /// Claim a project for this tree, e.g. `brayniac/rezolus` or `acme-corp/*`.
        #[arg(long)]
        project: Option<String>,
        /// Name this tree, for `waystation tree ls` and error messages.
        #[arg(long = "tree-name")]
        tree_name: Option<String>,
```

Update the destructuring in the `Setup` arm (`src/main.rs:182`):

```rust
        Cmd::Setup { operator, agent, swarm, project, tree_name, realm, remote, trust, subscribe } => {
```

Add before `cfg.validate()?;` in that arm:

```rust
            if tree_name.is_some() {
                cfg.tree.name = tree_name;
            }
            if let Some(p) = project
                && !cfg.tree.projects.iter().any(|c| c.eq_ignore_ascii_case(&p))
            {
                cfg.tree.projects.push(p);
            }
```

`setup` keeps acting on the tree `WAYSTATION_HOME` names — it loads and saves through `Config::load`/`save`, unchanged.

- [ ] **Step 6: Verify end to end**

```bash
cargo build
TMP=$(mktemp -d)
WAYSTATION_HOME="$TMP/oss" ./target/debug/waystation setup --operator brayniac \
  --tree-name oss --project brayniac/rezolus \
  --realm oss --remote git@github.com:brayniac/waystation-oss.git
WAYSTATION_HOME="$TMP/oss" ./target/debug/waystation setup --project brayniac/llm-perf
cat "$TMP/oss/config.toml"
```

Expected: the file contains

```toml
[tree]
name = "oss"
projects = ["brayniac/rezolus", "brayniac/llm-perf"]
```

- [ ] **Step 7: Run the whole suite**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`

Expected: PASS, no warnings.

- [ ] **Step 8: Commit**

```bash
git add src/tree.rs src/main.rs
git commit -m "cli: setup --project and --tree-name write the [tree] table"
```

---

### Task 8: Documentation

**Files:**
- Modify: `DESIGN.md` (new section after §3.2), `README.md` (new section after "Use from Claude Code"), `docs/superpowers/specs/2026-09-17-tree-launcher-design.md`

- [ ] **Step 1: Add the DESIGN.md section**

Insert after the §3.2 bullets (before `### 3.3 Tasks and claims`):

```markdown
### 3.2.1 Trees

A **tree** is one `WAYSTATION_HOME`: one identity, one set of mounted realms,
one clone set, one cursor file, and one daemon, since the socket lives inside
the tree. The default is `~/.waystation`.

One tree holds exactly one realm an operator can speak in: only one realm may
have `trust = "home"`, and posting to an external realm waits on §12.5. An
operator with separate worlds — personal, employer, a shared realm for
cross-over projects — therefore runs one tree per world.

Separate trees are preferred to several writable realms in one tree. A single
session holding two realms keeps both in one context window, where the only
barrier is guidance text; compaction, a summary, or a subagent can carry
content across it. Two trees are two processes with two clone sets, and nothing
in one is reachable from the other.

Each tree names itself and claims the projects that belong to it:

    [tree]
    name = "oss"
    projects = ["brayniac/rezolus", "brayniac/llm-perf"]

The default tree also lists the others in `siblings`, so the roster is one hop
from a fixed location. `waystation env` detects the project from the working
directory, resolves the single tree claiming it, and prints the environment.
Ambiguous claims are refused rather than tie-broken: every tie-break picks a
realm for traffic the operator believed was going elsewhere.
```

- [ ] **Step 2: Add the README section**

Insert after the "Manual alternative without the plugin" paragraph, before `### Hook fallback (no channel)`:

````markdown
### Multiple realms and trees

Mounting several realms in one config puts them in one session: useful for
reading, but only the `home` realm accepts posts. To hold two worlds you can
both speak in, run a **tree** per world. A tree is one `WAYSTATION_HOME` —
config, clones, cursors, and daemon:

```sh
WAYSTATION_HOME=~/.waystation-work waystation setup --operator <you> \
  --tree-name work --project <org>/'*' \
  --realm work --remote git@github.com:<org>/waystation-realm.git
WAYSTATION_HOME=~/.waystation-work waystation init
waystation tree add ~/.waystation-work
```

Each tree claims the repositories that belong to it, and `waystation env`
resolves the tree for the directory you are in:

```sh
$ cd ~/work/api-gateway && waystation env
export WAYSTATION_HOME=/home/you/.waystation-work
export WAYSTATION_PROJECT=<org>/api-gateway
```

Launch through a wrapper, in a subshell so nothing leaks back into your shell:

```sh
claude-ws() {
  ( eval "$(waystation env)"
    exec claude --dangerously-load-development-channels plugin:waystation@brayniac "$@" )
}
```

`waystation tree ls` shows the roster, what each tree claims, and which one
wins where you are standing. `--tree <name>` forces one. An inherited
`WAYSTATION_HOME` is deliberately ignored when resolving, so a shell that once
launched one tree does not keep selecting it.
````

- [ ] **Step 3: Correct the naming rule in the spec**

In `docs/superpowers/specs/2026-09-17-tree-launcher-design.md`, replace:

```
`name` labels the tree in listings and error messages. It is not identity, so
it does not belong in `[identity]`. A tree with no `name` falls back to the
basename of its directory.
```

with:

```
`name` labels the tree in listings and error messages. It is not identity, so
it does not belong in `[identity]`. A tree with no `name` is named after its
directory: a leading dot is dropped and a `waystation-` prefix is stripped, so
`~/.waystation-work` is `work` and `~/.waystation` is `default`.
```

- [ ] **Step 4: Check the docs against the build**

Run: `cargo run -- tree --help && cargo run -- env --help && cargo run -- setup --help`

Expected: the flags named in the docs all appear — `--tree`, `--tree-name`, `--project`, and the `tree ls|add|rm` subcommands.

- [ ] **Step 5: Commit**

```bash
git add DESIGN.md README.md docs/superpowers/specs/2026-09-17-tree-launcher-design.md
git commit -m "docs: trees, claims, and the env launcher"
```

---

## Verification

- [ ] `cargo test` — all tests pass
- [ ] `cargo clippy --all-targets -- -D warnings` — clean
- [ ] `cargo build && ./scripts/e2e.sh` — the existing two-agent end-to-end run still passes, confirming the config change did not disturb realm handling
- [ ] `waystation env` in a repository claimed by no tree prints the default tree
- [ ] `waystation tree ls` marks the tree that wins in the current directory
