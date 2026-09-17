//! Integration coverage for `waystation env`.
//!
//! `env_exports` (in `src/tree.rs`) is covered thoroughly by unit tests, but
//! nothing exercised `main.rs`'s `Cmd::Env` arm itself: `--root` defaulting,
//! `detect_project()` reading `WAYSTATION_PROJECT`, and `Roster::load`
//! reading real config files and siblings off disk. This is the integration
//! test DESIGN.md's spec calls for and nothing currently provides.
//!
//! Every invocation pins `WAYSTATION_HOME` to a scratch tempdir it never
//! reads or writes through — `env`/`tree` resolve entirely through `--root`
//! and ignore `WAYSTATION_HOME` — as a second line of defense against ever
//! touching the operator's real `~/.waystation`, matching `tests/setup_home.rs`.

use std::path::Path;
use std::process::{Command, Output};

fn waystation() -> Command {
    Command::new(env!("CARGO_BIN_EXE_waystation"))
}

fn run(cmd: &mut Command) -> Output {
    let out = cmd.output().expect("failed to spawn waystation");
    assert!(
        out.status.success(),
        "command failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Build a default tree plus a "work" sibling that claims `owner/repo`, and
/// register the sibling. Returns `(unused_waystation_home, default_dir, work_dir)`;
/// the first is only ever passed as `WAYSTATION_HOME` and never read from.
fn default_and_claiming_sibling() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let unused_home = tempfile::tempdir().unwrap();
    let default_dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();

    run(waystation()
        .env("WAYSTATION_HOME", unused_home.path())
        .args(["setup", "--home", path_str(default_dir.path()), "--operator", "brayniac"]));

    run(waystation().env("WAYSTATION_HOME", unused_home.path()).args([
        "setup",
        "--home",
        path_str(work_dir.path()),
        "--operator",
        "brayniac",
        "--tree-name",
        "work",
        "--project",
        "owner/repo",
    ]));

    run(waystation().env("WAYSTATION_HOME", unused_home.path()).args([
        "--root",
        path_str(default_dir.path()),
        "tree",
        "add",
        path_str(work_dir.path()),
    ]));

    (unused_home, default_dir, work_dir)
}

fn path_str(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn env_resolves_a_claimed_project_to_the_claiming_tree() {
    let (unused_home, default_dir, work_dir) = default_and_claiming_sibling();

    let out = run(waystation()
        .env("WAYSTATION_HOME", unused_home.path())
        .env("WAYSTATION_PROJECT", "owner/repo")
        .args(["--root", path_str(default_dir.path()), "env"]));

    let stdout = String::from_utf8_lossy(&out.stdout);
    let expected = format!(
        "export WAYSTATION_HOME='{}'\nexport WAYSTATION_PROJECT='owner/repo'\n",
        work_dir.path().display()
    );
    assert_eq!(stdout, expected, "a session in owner/repo must resolve to the claiming `work` tree");
}

#[test]
fn env_falls_back_to_the_default_tree_for_an_unclaimed_project() {
    let (unused_home, default_dir, _work_dir) = default_and_claiming_sibling();

    let out = run(waystation()
        .env("WAYSTATION_HOME", unused_home.path())
        .env("WAYSTATION_PROJECT", "someone-else/unclaimed")
        .args(["--root", path_str(default_dir.path()), "env"]));

    let stdout = String::from_utf8_lossy(&out.stdout);
    let expected = format!(
        "export WAYSTATION_HOME='{}'\nexport WAYSTATION_PROJECT='someone-else/unclaimed'\n",
        default_dir.path().display()
    );
    assert_eq!(
        stdout, expected,
        "a project no sibling claims must fall back to the default tree, not the claiming one"
    );
}
