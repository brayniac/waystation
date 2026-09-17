//! Trees: one `WAYSTATION_HOME` each, selected by the project a session runs in.

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
}
