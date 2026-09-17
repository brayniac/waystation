//! Trees: one `WAYSTATION_HOME` each, selected by the project a session runs in.

/// Does `claim` cover `project`? Exact `owner/name`, or an `owner/*` glob.
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
