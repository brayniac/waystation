# Trees and `waystation env`

Design, 2026-09-17.

## Problem

A session must choose which realm world it belongs to before it starts, and
today that choice is an environment variable the operator sets by hand.

Two rules in the current implementation combine into the constraint that makes
the choice necessary. Exactly one realm per config may have `trust = "home"`;
`Config::validate` rejects a second (`src/config.rs:227-236`). Posting to an
external realm is refused outright until the two-phase confirm flow of
DESIGN.md §12.5 lands (`src/core.rs:199-203`). Together they mean one config
gives the operator exactly one realm they can speak in. Everything else mounted
alongside it is readable only.

An operator with a personal realm, a work realm, and a shared realm for
cross-over projects therefore needs three separate configs, selected per
session through `WAYSTATION_HOME`. Selecting by hand is error-prone in the one
direction that matters: launching from the wrong shell puts work traffic in a
personal realm, or the reverse.

## The tree as a concept

`WAYSTATION_HOME` already relocates every piece of local state: the config
(`src/config.rs:133`), the per-session clones (`src/config.rs:210-219`), the
inbox cursors (`src/config.rs:273`), and the daemon socket
(`src/backend.rs:132-134`). One such directory is a **tree**. The default is
`~/.waystation`.

Because the socket is inside the tree, two trees run two daemons that share no
clone, no cursor, and no process. A session mounts exactly one tree and cannot
observe another. This is the property the design is built to preserve:
isolation between realm worlds is enforced by the filesystem and the process
table, not by instructions the model is asked to honour.

That property is why several trees are preferred over several writable realms
in one tree. A single session holding both a work realm and a personal realm
keeps both in one context window, where the only barrier is the guidance text
appended to the server's instructions (`src/core.rs:284-295`). Compaction, a
summary, or a dispatched subagent can carry content across such a barrier. A
process boundary has no equivalent failure.

Trees remain the isolation boundary regardless of what happens to §12.5. If
external realms become writable, or the one-home rule is relaxed, an operator
may choose to mount more in one tree; nothing in this design prevents it or
depends on the restriction staying.

## Configuration

Each tree declares itself and the projects it claims:

```toml
[tree]
name = "oss"
projects = ["brayniac/rezolus", "brayniac/llm-perf"]
```

`name` labels the tree in listings and error messages. It is not identity, so
it does not belong in `[identity]`. A tree with no `name` is named after its
directory: a leading dot is dropped and a `waystation-` prefix is stripped, so
`~/.waystation-work` is `work` and `~/.waystation` is `default`. A directory
named exactly `waystation-` (nothing left after the prefix is stripped) is
also `default`, not a tree literally named `waystation-`.

`projects` holds `owner/name` entries as `detect_project` produces them, and
`owner/*` globs. Claims live beside the realm they describe, so no separate
file duplicates the mapping and nothing can drift out of step.

The default tree additionally holds the roster of the others:

```toml
[tree]
name = "home"
siblings = ["~/.waystation-work", "~/.waystation-oss"]
```

Paths only. Each tree still owns its own claims. `~` expands at load.

The **roster** is the default tree together with every path it lists. The
default tree is a tree like any other: it may claim projects, and it is what
resolution falls back to when nothing claims the current one.

## Resolution

`waystation env` resolves a tree in this order:

1. `--tree <name>` names a tree in the roster. Explicit selection always wins.
   An unknown name is an error listing the known names.
2. Otherwise the project is detected: `WAYSTATION_PROJECT` if set, else the
   `origin` remote of the working directory, reusing `detect_project`
   (`src/core.rs:23-40`) so that the launcher and the running server can never
   disagree about which repository the session is in.
3. The roster is scanned for trees claiming that project, exact matches and
   globs alike.
4. Exactly one claimant wins. No claimant resolves to the default tree. Two or
   more is an error naming every claimant and the project.
5. A directory that is not a repository, or has no `origin`, resolves to the
   default tree.

Ambiguity is refused rather than broken by a tie-break rule. Two trees claiming
one project is a configuration mistake, and every possible tie-break resolves it
by silently choosing a realm for traffic the operator believed was going
somewhere else.

Resolution always begins at `~/.waystation`, and an inherited `WAYSTATION_HOME`
is ignored. A variable that persists in a shell is the failure this feature
exists to remove: a shell that once launched a work session would otherwise keep
resolving to the work tree from every other repository. `--tree` is the
supported way to force a choice.

A hidden `--root <path>` overrides the starting directory so tests do not touch
the operator's home.

## Output

```
$ waystation env
export WAYSTATION_HOME=/Users/brian/.waystation-oss
export WAYSTATION_PROJECT=brayniac/rezolus
```

`WAYSTATION_PROJECT` is emitted as well, pinning the project that resolution
actually used. The server would otherwise detect it again from whatever
directory it is started in, which need not be the directory the operator
launched from.

POSIX `export` lines are the only format. The documented wrapper evaluates them
in a subshell so that nothing leaks back into the interactive shell:

```sh
claude-ws() {
  ( eval "$(waystation env)"
    exec claude --dangerously-load-development-channels plugin:waystation@brayniac "$@" )
}
```

Operators of shells that do not accept this syntax read the two values and set
them in their own syntax. If that proves to be a real burden, a format flag is
the smallest possible addition; it is not worth carrying before someone asks.

## Commands

```
waystation env [--tree <name>]           resolve, print exports
waystation tree ls                       roster, claims, default, and the winner here
waystation tree add <path>               add a tree to the roster
waystation tree rm <path>                remove one
waystation setup --project <owner/name>  append a claim to this tree
```

`tree add` and `tree rm` edit the roster in the default tree, whatever
`WAYSTATION_HOME` currently points at, because that is where the roster lives.

Claims go through `setup`, which already writes identity and realms, rather
than through a new verb. `setup` continues to act on the tree that
`WAYSTATION_HOME` names, so a claim is added to the tree being configured. The
rule that resolution ignores an inherited `WAYSTATION_HOME` governs `env`
alone; it does not change how the configuring commands choose a target.

`tree ls` warns about a roster entry that is missing or holds no `config.toml`,
and resolution skips it. A stale entry must not break the launcher.

## Testing

Resolution is a pure function from a roster and a project to a tree, tested as
a table: exact match, glob match, no match, ambiguity, `--tree` override, and a
directory that is not a repository. `--root` points the table at a temporary
directory. Remote-to-project parsing is already covered by
`project_from_remote` (`src/core.rs:43`), so the table needs no git.

One integration test asserts the exact output of `env` for a resolved tree.

## Documentation

DESIGN.md gains a section on trees: the isolation model, one writable realm per
tree, why several trees are preferred to several writable realms in one, and
the daemon-per-tree consequence.

README gains "Multiple realms and trees", covering the distinction between
mounting a realm and running a separate tree, the claim configuration, and the
wrapper above.

## Not in scope

No shell hook and no directory-change integration. No `exec` subcommand; the
wrapper covers the shells in use, and `env` composes with anything that needs
more. No messaging between trees, and no change to the one-home rule or to the
§12.5 confirm flow.
