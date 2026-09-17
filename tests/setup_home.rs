//! Regression test for the incident that motivated `setup --home`: a run of
//! `waystation setup` with no explicit target silently wrote into the
//! operator's real `~/.waystation`, next to a running daemon. `--home` must
//! let an operator name the tree to configure, and must never touch
//! `$WAYSTATION_HOME` (or any other tree) when it does.

use std::process::Command;

fn waystation() -> Command {
    Command::new(env!("CARGO_BIN_EXE_waystation"))
}

#[test]
fn setup_home_writes_the_named_tree_and_leaves_waystation_home_untouched() {
    let env_home = tempfile::tempdir().unwrap();
    let target_home = tempfile::tempdir().unwrap();

    let output = waystation()
        .env("WAYSTATION_HOME", env_home.path())
        .args([
            "setup",
            "--home",
            target_home.path().to_str().unwrap(),
            "--operator",
            "brayniac",
            "--tree-name",
            "second-tree",
        ])
        .output()
        .expect("failed to run waystation setup --home");

    assert!(
        output.status.success(),
        "setup --home failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The named tree got the config...
    let target_config = target_home.path().join("config.toml");
    assert!(target_config.exists(), "setup --home did not write {}", target_config.display());
    let written = std::fs::read_to_string(&target_config).unwrap();
    assert!(written.contains("brayniac"), "config missing operator: {written}");
    assert!(written.contains("second-tree"), "config missing tree name: {written}");

    // ...and the tree WAYSTATION_HOME pointed at was never written to. This
    // is the exact failure mode from the incident: an omitted --home (or an
    // unset WAYSTATION_HOME) must not fall through to a real tree.
    let env_config = env_home.path().join("config.toml");
    assert!(
        !env_config.exists(),
        "setup --home must not write $WAYSTATION_HOME's config, but {} exists",
        env_config.display()
    );

    // stdout names the directory actually written, not some other tree.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(target_home.path().to_str().unwrap()),
        "expected `wrote {{path}}` to name the --home directory, got: {stdout}"
    );
}

#[test]
fn setup_project_scp_remote_normalizes_to_owner_name() {
    let home = tempfile::tempdir().unwrap();

    let output = waystation()
        .env("WAYSTATION_HOME", home.path())
        .args(["setup", "--home", home.path().to_str().unwrap(), "--project", "git@github.com:owner/repo.git"])
        .output()
        .expect("failed to run waystation setup --project");

    assert!(output.status.success());
    let written = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(written.contains("owner/repo"), "expected the SCP remote normalized to owner/repo: {written}");
    assert!(!written.contains("git@github.com"), "raw SCP form must not be stored verbatim: {written}");
}

#[test]
fn setup_bare_project_claim_is_stored_but_warns_on_stderr() {
    let home = tempfile::tempdir().unwrap();

    let output = waystation()
        .env("WAYSTATION_HOME", home.path())
        .args(["setup", "--home", home.path().to_str().unwrap(), "--project", "rezolus"])
        .output()
        .expect("failed to run waystation setup --project");

    assert!(output.status.success());
    let written = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(written.contains("rezolus"), "bare claim should still be stored: {written}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rezolus") && stderr.contains('/'),
        "expected a stderr warning about the bare claim not matching a detected repository, got: {stderr}"
    );
}

#[test]
fn setup_without_home_defaults_to_waystation_home() {
    let env_home = tempfile::tempdir().unwrap();

    let output = waystation()
        .env("WAYSTATION_HOME", env_home.path())
        .args(["setup", "--operator", "brayniac"])
        .output()
        .expect("failed to run waystation setup");

    assert!(output.status.success());
    let config = env_home.path().join("config.toml");
    assert!(config.exists(), "setup with no --home should still fall back to $WAYSTATION_HOME");
}
