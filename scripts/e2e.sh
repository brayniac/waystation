#!/usr/bin/env bash
# Two agents coordinating through a local bare repo. Usage: scripts/e2e.sh [workdir]
set -euo pipefail
S="${1:-$(mktemp -d)}"; WS="${WS:-$(dirname "$0")/../target/debug/waystation}"
mkdir -p "$S"; git init -q --bare "$S/remote.git"
A="WAYSTATION_HOME=$S/a"; B="WAYSTATION_HOME=$S/b"
env $A "$WS" setup --operator acme --agent alpha --realm home --remote "$S/remote.git" --subscribe general,backend >/dev/null
env $A "$WS" init >/dev/null
env $A "$WS" register --role planner --focus "e2e" >/dev/null
env $B "$WS" setup --operator acme --agent beta --realm home --remote "$S/remote.git" --subscribe general >/dev/null
env $B "$WS" init >/dev/null; env $B "$WS" sync >/dev/null
echo "## A -> B direct (immediate) and a low-priority note (silent)"
env $A "$WS" post backend --priority high --to acme/beta --body "beta, please review the poller"
env $A "$WS" post general --priority low --body "fyi low prio"
echo "## B inbox --hook"; env $B "$WS" inbox --hook; echo
echo "## B inbox --include-silent"; env $B "$WS" inbox --include-silent
echo "## B replies"; ID=$(env $B "$WS" read backend --limit 1 | sed -n 's/^\[\([0-9A-Z]*\)\].*/\1/p')
env $B "$WS" post backend --reply-to "$ID" --body "on it"
echo "## A inbox (reply to A's thread is immediate)"; env $A "$WS" inbox
echo "## concurrent posts (push race → rebase → retry)"
( env $A "$WS" post general --body "race A" ) & ( env $B "$WS" post general --body "race B" ) & wait
echo "## read general"; env $A "$WS" read general
echo "## agents"; env $A "$WS" agents
echo "## remote log"; git --git-dir "$S/remote.git" log --oneline | cat
for h in a b; do env WAYSTATION_HOME=$S/$h "$WS" daemon stop >/dev/null 2>&1 || true; done
echo "workdir: $S"
