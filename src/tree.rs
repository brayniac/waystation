//! Trees: one `WAYSTATION_HOME` each, selected by the project a session runs in.

use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// One tree in the roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// The tree's name, as configured; unique among trees a roster can resolve.
    pub name: String,
    /// The tree's `WAYSTATION_HOME` directory.
    pub path: PathBuf,
    /// `owner/name` entries, as `detect_project` produces them, and `owner/*` globs.
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
    /// Read the tree at `root` and every sibling it lists.
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
            if roster.trees.iter().any(|t| same_tree(&t.path, &path)) {
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
            let sc = match Config::load_from(&path) {
                Ok(sc) => sc,
                Err(e) => {
                    roster.warnings.push(format!(
                        "tree {} could not be read ({e}); skipped. Remove it with `waystation tree rm {}`",
                        path.display(),
                        sibling.display()
                    ));
                    continue;
                }
            };
            roster.trees.push(Tree {
                name: sc.tree.name.clone().unwrap_or_else(|| name_from_path(&path)),
                path,
                projects: sc.tree.projects.clone(),
                is_default: false,
            });
        }
        Ok(roster)
    }

    pub fn default_tree(&self) -> Result<&Tree> {
        self.trees.iter().find(|t| t.is_default).context("no default tree")
    }

    /// Pick a tree: a forced name wins, else the one claiming `project`, else the default.
    pub fn resolve(&self, project: Option<&str>, forced: Option<&str>) -> Result<&Tree> {
        if let Some(name) = forced {
            let named: Vec<&Tree> = self.trees.iter().filter(|t| t.name == name).collect();
            return match named.as_slice() {
                [] => bail!(
                    "unknown tree `{name}`; known trees: {}",
                    self.trees.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
                ),
                [one] => Ok(*one),
                many => bail!(
                    "tree name `{name}` is ambiguous: it names more than one tree ({}); \
                     rename all but one so tree names are unique",
                    many.iter()
                        .map(|t| format!("{} at {}", t.name, t.path.display()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
        }
        let Some(project) = project else { return self.default_tree() };
        let claimants: Vec<&Tree> = self
            .trees
            .iter()
            .filter(|t| t.projects.iter().any(|c| claim_matches(c, project)))
            .collect();
        match claimants.as_slice() {
            [] => {
                // A skipped tree could be the one that would have claimed this
                // project; falling back to the default here would silently
                // misroute a work session into it. Refuse instead, unless
                // nothing was skipped.
                if !self.warnings.is_empty() {
                    bail!(
                        "project `{project}` matched no readable tree, and {} tree(s) could not \
                         be read and might have claimed it ({}); fix the broken tree's config, \
                         remove it with `waystation tree rm <path>`, or choose deliberately with \
                         --tree <name>",
                        self.warnings.len(),
                        self.warnings.join("; ")
                    );
                }
                self.default_tree()
            }
            [one] => Ok(one),
            many => bail!(
                "project `{project}` is claimed by more than one tree ({}); \
                 remove the claim from all but one, or name the tree with --tree",
                many.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// Does `claim` cover `project`? Exact `owner/name`, or an `owner/*` glob.
/// Comparison ignores surrounding whitespace and ASCII case.
pub fn claim_matches(claim: &str, project: &str) -> bool {
    let claim = claim.trim().to_lowercase();
    let project = project.trim().to_lowercase();
    if let Some(owner) = claim.strip_suffix("/*") {
        // An empty owner is not a wildcard: "/*" must not match everything.
        if owner.is_empty() {
            return false;
        }
        return project
            .split_once('/')
            .is_some_and(|(o, rest)| o == owner && !rest.is_empty());
    }
    claim == project
}

/// Expand a leading `~` against the home directory. Only a bare leading `~`
/// component is expanded; `~user/...`-style paths are left alone unchanged,
/// since these are config-file strings, not shell input, so shell-style
/// `~user` expansion does not apply.
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
        // "waystation", "waystation-" (empty remainder), and no basename at all
        // are all the default tree, not a tree literally named "waystation-".
        _ if base == "waystation" || base == "waystation-" || base.is_empty() => {
            "default".to_string()
        }
        _ => base,
    }
}

/// Add `path` to the default tree's roster. Returns false if it was already there.
///
/// The operator's spelling of `path` is what gets stored, not its expanded
/// form — see `stored_spelling`. `expanded` is only for the existence check,
/// the default-tree check, and the dedup.
pub fn roster_add(root: &Path, path: &Path) -> Result<bool> {
    let root = expand_tilde(root);
    let expanded = expand_tilde(path);
    if !expanded.join("config.toml").exists() {
        bail!(
            "{} has no config.toml; set the tree up first with \
             `WAYSTATION_HOME={} waystation setup ...`",
            expanded.display(),
            expanded.display()
        );
    }
    if expanded == root {
        bail!("{} is the default tree; it is always in the roster", expanded.display());
    }
    let mut cfg = Config::load_from(&root)?;
    if cfg.tree.siblings.iter().any(|s| same_tree(s, &expanded)) {
        return Ok(false);
    }
    cfg.tree.siblings.push(stored_spelling(path, &expanded));
    cfg.save_to(&root)?;
    Ok(true)
}

/// Remove `path` from the default tree's roster. Returns false if it was absent.
pub fn roster_rm(root: &Path, path: &Path) -> Result<bool> {
    let root = expand_tilde(root);
    let path = expand_tilde(path);
    let mut cfg = Config::load_from(&root)?;
    let before = cfg.tree.siblings.len();
    cfg.tree.siblings.retain(|s| !same_tree(s, &path));
    if cfg.tree.siblings.len() == before {
        return Ok(false);
    }
    cfg.save_to(&root)?;
    Ok(true)
}

/// What to persist for a sibling the operator named. Keeps a `~` or absolute
/// spelling as written, so a roster stays portable across machines; absolutizes
/// a relative one, which would otherwise resolve against whatever directory a
/// later command runs in.
fn stored_spelling(given: &Path, expanded: &Path) -> PathBuf {
    if given.starts_with("~") || given.is_absolute() {
        given.to_path_buf()
    } else {
        expanded.canonicalize().unwrap_or_else(|_| expanded.to_path_buf())
    }
}

/// Do two paths name the same tree? Compares canonically when both paths
/// resolve, so `..` segments and symlinks do not create duplicate entries,
/// and falls back to comparing the expanded paths when they do not.
fn same_tree(a: &Path, b: &Path) -> bool {
    let (a, b) = (expand_tilde(a), expand_tilde(b));
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Quote a value for POSIX `sh` so `eval` sees exactly one literal word.
/// A single quote is closed, escaped, and reopened — the standard `'\''` dance.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The shell lines `waystation env` prints for a resolved tree.
///
/// The caller's only consumer is `eval "$(waystation env)"`, so every value is
/// single-quoted for `sh` before it is emitted. A control character (e.g. a
/// newline) survives quoting unharmed but is still refused: something that can
/// smuggle extra lines past a human skimming the output is pathological,
/// whatever its source (a tree path, or a project parsed from a git remote
/// URL that is not under this tool's control).
pub fn env_exports(tree: &Tree, project: Option<&str>) -> Result<String> {
    let path = tree.path.to_string_lossy();
    if path.chars().any(|c| c.is_control()) {
        bail!("tree path {path:?} contains a control character; refusing to emit it for `eval`");
    }
    let mut out = format!("export WAYSTATION_HOME={}\n", sh_quote(&path));
    if let Some(p) = project {
        if p.chars().any(|c| c.is_control()) {
            bail!("project {p:?} contains a control character; refusing to emit it for `eval`");
        }
        out.push_str(&format!("export WAYSTATION_PROJECT={}\n", sh_quote(p)));
    }
    Ok(out)
}

/// Add a project claim, unless an equal one is already present.
/// Returns whether it was added. `setup` calls this on the config it already holds.
pub fn add_claim(projects: &mut Vec<String>, project: &str) -> bool {
    if projects.iter().any(|c| c.eq_ignore_ascii_case(project)) {
        return false;
    }
    projects.push(project.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

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
        // Case-insensitivity applies to the glob branch too.
        assert!(claim_matches("Acme-Corp/*", "acme-corp/api-gateway"));
        // An empty owner is not a wildcard; "/*" must not match everything.
        assert!(!claim_matches("/*", "/x"));
    }

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

    #[test]
    fn no_default_tree_is_an_error() {
        let r = Roster { trees: vec![tree("work", &[], false)], warnings: vec![] };
        assert!(r.resolve(None, None).is_err());
    }

    #[test]
    fn empty_roster_is_an_error_not_a_panic() {
        let r = Roster::default();
        assert!(r.resolve(None, None).is_err());
        assert!(r.resolve(Some("brayniac/rezolus"), None).is_err());
        assert!(r.resolve(None, Some("work")).is_err());
    }

    #[test]
    fn duplicate_tree_names_are_an_error_naming_both_paths() {
        let mut r = roster();
        r.trees.push(Tree {
            path: PathBuf::from("/trees/second"),
            ..tree("work", &[], false)
        });
        let err = r.resolve(None, Some("work")).unwrap_err().to_string();
        assert!(err.contains("/trees/work"), "{err}");
        assert!(err.contains("/trees/second"), "{err}");
    }

    fn roster_with_warning() -> Roster {
        let mut r = roster();
        r.warnings.push("tree /trees/broken could not be read (parse error)".into());
        r
    }

    #[test]
    fn unclaimed_project_with_warnings_present_is_an_error() {
        let r = roster_with_warning();
        let err = r.resolve(Some("someone/else"), None).unwrap_err().to_string();
        assert!(err.contains("/trees/broken"), "{err}");
        assert!(err.contains("tree rm"), "{err}");
        assert!(err.contains("--tree"), "{err}");
    }

    #[test]
    fn unclaimed_project_with_no_warnings_still_falls_back_to_default() {
        let r = roster();
        assert_eq!(r.resolve(Some("someone/else"), None).unwrap().name, "default");
    }

    #[test]
    fn no_project_at_all_ignores_warnings() {
        let r = roster_with_warning();
        assert_eq!(r.resolve(None, None).unwrap().name, "default");
    }

    #[test]
    fn claimed_project_ignores_warnings() {
        let r = roster_with_warning();
        assert_eq!(r.resolve(Some("brayniac/rezolus"), None).unwrap().name, "oss");
    }

    #[test]
    fn forced_tree_ignores_warnings() {
        let r = roster_with_warning();
        assert_eq!(r.resolve(Some("someone/else"), Some("work")).unwrap().name, "work");
    }

    #[test]
    fn expand_tilde_leaves_plain_paths_alone() {
        assert_eq!(expand_tilde(Path::new("/trees/work")), PathBuf::from("/trees/work"));
    }

    #[test]
    fn expand_tilde_leaves_shell_style_user_tilde_alone() {
        assert_eq!(expand_tilde(Path::new("~user/foo")), PathBuf::from("~user/foo"));
    }

    #[test]
    fn name_from_path_empty_remainder_after_prefix_is_also_default() {
        assert_eq!(name_from_path(Path::new("/Users/x/.waystation-")), "default");
    }

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
        write_tree(&root, None, &[], std::slice::from_ref(&work));
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
        write_tree(&root, None, &[], std::slice::from_ref(&gone));

        let r = Roster::load(&root).unwrap();
        assert_eq!(r.trees.len(), 1);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains(".waystation-gone"), "{:?}", r.warnings);
    }

    #[test]
    fn a_sibling_with_unparseable_config_warns_and_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        let broken = tmp.path().join(".waystation-broken");
        write_tree(&root, None, &[], std::slice::from_ref(&broken));
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("config.toml"), "not valid toml {{{").unwrap();

        let r = Roster::load(&root).unwrap();
        assert_eq!(r.trees.len(), 1);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains(".waystation-broken"), "{:?}", r.warnings);
        assert!(r.warnings[0].contains("tree rm"), "{:?}", r.warnings);
    }

    #[test]
    fn a_default_tree_with_unparseable_config_fails_loudly() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("config.toml"), "not valid toml {{{").unwrap();

        assert!(Roster::load(&root).is_err());
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

    #[test]
    fn stored_spelling_keeps_a_tilde_spelling_unchanged() {
        // Regression test for the bug where roster_add stored the expanded,
        // machine-specific path instead of the operator's `~` spelling. This
        // needs no real home directory: the `~` branch never looks at `expanded`.
        let given = Path::new("~/.waystation-work");
        let expanded = Path::new("/wherever/this/machine/keeps/home/.waystation-work");
        assert_eq!(stored_spelling(given, expanded), given);
    }

    #[test]
    fn stored_spelling_keeps_an_absolute_spelling_unchanged() {
        let given = Path::new("/trees/work");
        assert_eq!(stored_spelling(given, given), given);
    }

    #[test]
    fn stored_spelling_absolutizes_a_relative_spelling() {
        // A relative path must not be stored as given: it would resolve
        // against whatever directory a later command happens to run in.
        let cwd = std::env::current_dir().unwrap();
        let tmp = tempfile::tempdir_in(&cwd).unwrap();
        let name = tmp.path().file_name().unwrap();
        let relative = PathBuf::from(name);
        // expand_tilde leaves a relative, non-tilde path unchanged, so
        // `expanded` is the same relative path in this case too.
        let stored = stored_spelling(&relative, &relative);
        assert!(stored.is_absolute(), "{stored:?}");
        assert_eq!(stored, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn same_tree_dedups_across_different_spellings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        let work = tmp.path().join(".waystation-work");
        write_tree(&root, None, &[], &[]);
        write_tree(&work, Some("work"), &["acme-corp/*"], &[]);

        assert!(roster_add(&root, &work).unwrap());

        // Same tree, spelled by routing through `..` (through a directory that
        // actually exists, so canonicalize can resolve it).
        let via_dotdot = work.join("..").join(".waystation-work");
        assert!(
            !roster_add(&root, &via_dotdot).unwrap(),
            "adding the same tree via a `..` spelling is a no-op"
        );
        assert_eq!(Roster::load(&root).unwrap().trees.len(), 2);

        assert!(
            roster_rm(&root, &via_dotdot).unwrap(),
            "rm with the `..` spelling finds the entry added under the plain path"
        );
        assert_eq!(Roster::load(&root).unwrap().trees.len(), 1);
    }

    #[test]
    fn resolve_refuses_to_misroute_when_a_real_sibling_is_broken() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".waystation");
        let broken = tmp.path().join(".waystation-broken");
        write_tree(&root, None, &[], std::slice::from_ref(&broken));
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("config.toml"), "not valid toml {{{").unwrap();

        let roster = Roster::load(&root).unwrap();
        assert_eq!(roster.trees.len(), 1);
        assert_eq!(roster.warnings.len(), 1);

        let err = roster.resolve(Some("someone/else"), None).unwrap_err().to_string();
        assert!(err.contains("could not be read"), "{err}");
        assert!(err.contains("tree rm"), "{err}");
    }

    #[test]
    fn exports_name_the_tree_and_the_project() {
        let t = tree("oss", &[], false);
        let out = env_exports(&t, Some("brayniac/rezolus")).unwrap();
        assert_eq!(
            out,
            "export WAYSTATION_HOME='/trees/oss'\nexport WAYSTATION_PROJECT='brayniac/rezolus'\n"
        );
    }

    #[test]
    fn exports_omit_the_project_when_there_is_none() {
        let t = tree("default", &[], true);
        assert_eq!(env_exports(&t, None).unwrap(), "export WAYSTATION_HOME='/trees/default'\n");
    }

    #[test]
    fn exports_quote_a_project_that_looks_like_a_shell_injection() {
        let t = tree("oss", &[], false);
        let out = env_exports(&t, Some("acme/pwn;touch PWNED")).unwrap();
        assert_eq!(
            out,
            "export WAYSTATION_HOME='/trees/oss'\nexport WAYSTATION_PROJECT='acme/pwn;touch PWNED'\n"
        );
    }

    #[test]
    fn exports_quote_a_path_containing_a_space() {
        let t = Tree {
            path: PathBuf::from("/Users/x/Library/CloudStorage/My Drive/.waystation"),
            ..tree("default", &[], true)
        };
        let out = env_exports(&t, None).unwrap();
        assert_eq!(
            out,
            "export WAYSTATION_HOME='/Users/x/Library/CloudStorage/My Drive/.waystation'\n"
        );
    }

    #[test]
    fn exports_escape_a_single_quote_in_a_value() {
        let t = tree("oss", &[], false);
        let out = env_exports(&t, Some("brayniac/it's-fine")).unwrap();
        assert_eq!(
            out,
            "export WAYSTATION_HOME='/trees/oss'\nexport WAYSTATION_PROJECT='brayniac/it'\\''s-fine'\n"
        );
    }

    #[test]
    fn exports_refuse_a_project_with_a_control_character() {
        let t = tree("oss", &[], false);
        let err = env_exports(&t, Some("brayniac/rezolus\nexport EVIL=1")).unwrap_err().to_string();
        assert!(err.contains("control character"), "{err}");
    }

    #[test]
    fn exports_refuse_a_path_with_a_control_character() {
        let t = Tree { path: PathBuf::from("/trees/oss\nexport EVIL=1"), ..tree("oss", &[], false) };
        let err = env_exports(&t, None).unwrap_err().to_string();
        assert!(err.contains("control character"), "{err}");
    }

    /// Not just that we quoted the way we meant to — that the quoting actually
    /// survives a real shell's `eval`, for both the injection and the space case.
    #[test]
    fn exports_round_trip_through_a_real_shell_eval() {
        for project in ["acme/pwn;touch PWNED", "brayniac/has space"] {
            let t = tree("oss", &[], false);
            let out = env_exports(&t, Some(project)).unwrap();
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(r#"eval "$1"; printf %s "$WAYSTATION_PROJECT""#)
                .arg("--")
                .arg(&out)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
            let recovered = String::from_utf8(output.stdout).unwrap();
            assert_eq!(recovered, project, "round-trip failed for {project:?}: exports were {out:?}");
        }
    }

    #[test]
    fn claiming_a_project_is_idempotent() {
        let mut projects: Vec<String> = vec![];
        assert!(add_claim(&mut projects, "brayniac/rezolus"));
        assert!(!add_claim(&mut projects, "brayniac/rezolus"));
        assert!(!add_claim(&mut projects, "Brayniac/Rezolus"), "claims are case-insensitive");
        assert!(add_claim(&mut projects, "brayniac/llm-perf"));
        assert_eq!(
            projects,
            vec!["brayniac/rezolus".to_string(), "brayniac/llm-perf".to_string()]
        );
    }
}
