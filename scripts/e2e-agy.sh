#!/usr/bin/env bash
# End-to-end smoke test for the Antigravity CLI (`agy`) — the sibling of
# scripts/e2e.sh and scripts/e2e-copilot.sh.
#
# Drives a real `agy -p` against a project wired by `thirdeye install -a
# antigravity <project>`: hooks that name no event and no turn, a view, an edit
# with its range, a write, a shell write observed, a flag through the MCP
# server, and — Antigravity having no session-end hook — the daemon noticing
# agy exit and closing its kitty split by itself. Facts it relies on:
# docs/antigravity-hook-payload.md.
#
# The install also allows mcp(thirdeye/flag) in
# ~/.gemini/antigravity-cli/settings.json, and a project uninstall leaves it.
#
# Model: $THIRDEYE_AGY_MODEL, default gemini-3.8-flash-low.
#
# Usage:  scripts/e2e-agy.sh [--no-kitty]

set -uo pipefail

NO_KITTY=0
[ "${1:-}" = "--no-kitty" ] && NO_KITTY=1

MODEL="${THIRDEYE_AGY_MODEL:-gemini-3.8-flash-low}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}"
mkdir -p "$CACHE"
SCRATCH="$(mktemp -d "$CACHE/thirdeye-e2e-agy-XXXXXX")"
chmod 700 "$SCRATCH"
PROJECT="$SCRATCH/project"
KITTY_SOCKET="/tmp/kitty-e2e-agy-$$"
KITTY_PID=""
FAILURES=0
SOFT=0
STATE_ROOT="${XDG_STATE_HOME:-$HOME/.local/state}/thirdeye"

# The agent must reach only the kitty this script launches (see e2e.sh), and
# none of an enclosing agent's session.
unset TMUX TMUX_PANE THIRDEYE_SURFACE KITTY_LISTEN_ON
UNSET=()
for v in $(compgen -e | grep -E '^(CLAUDE|COPILOT_AGENT|ANTIGRAVITY)'); do UNSET+=(-u "$v"); done

pass() { printf '  \033[32mok\033[0m    %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }
soft() { printf '  \033[33msoft\033[0m  %s\n' "$1"; SOFT=$((SOFT + 1)); }
step() { printf '\n== %s\n' "$1"; }

SESSION=""
cleanup() {
    step "cleaning up"
    if [ -n "$SESSION" ]; then
        systemctl --user stop "thirdeye@$SESSION.socket" "thirdeye@$SESSION.service" >/dev/null 2>&1
        rm -rf "$STATE_ROOT"/*/"$SESSION"
        rm -rf "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/thirdeye/$SESSION."*
    fi
    [ -n "$KITTY_PID" ] && kill "$KITTY_PID" >/dev/null 2>&1
    cp "$SCRATCH/agy.log" "$CACHE/thirdeye-e2e-agy-last.log" 2>/dev/null \
        && echo "  agy's output: ${CACHE/#$HOME/\~}/thirdeye-e2e-agy-last.log"
    rm -rf "$SCRATCH" "$KITTY_SOCKET"
    for ws in "$STATE_ROOT"/project-*/; do
        [ -d "$ws" ] || continue
        [ "$(find "$ws" -mindepth 1 -maxdepth 1 -type d | wc -l)" = 0 ] && rm -rf "$ws"
    done
    echo "  removed the scratch project, session state and units"
}
trap cleanup EXIT

# --- preconditions -------------------------------------------------------
step "preconditions"
command -v thirdeye >/dev/null || { fail "thirdeye is not on PATH — cargo xtask install"; exit 1; }
command -v agy >/dev/null || { fail "agy is not on PATH"; exit 1; }
pass "thirdeye: $(thirdeye --version)"
pass "agy: $(agy --version 2>/dev/null | head -1), model $MODEL"
if [ "$NO_KITTY" = 0 ]; then
    command -v kitty >/dev/null || { fail "kitty is not on PATH (use --no-kitty)"; exit 1; }
fi

# --- a scratch project ---------------------------------------------------
step "scratch project at ${PROJECT/#$HOME/\~}"
mkdir -p "$PROJECT"
printf 'alpha\nbeta\ngamma\n' > "$PROJECT/data.txt"
git -C "$PROJECT" init -q && pass "a git repository, so observation is bounded by git"
thirdeye install -a antigravity "$PROJECT" >/dev/null \
    && [ -f "$PROJECT/.agents/hooks.json" ] \
    && pass "thirdeye install antigravity wrote .agents/" \
    || { fail "thirdeye install antigravity failed"; exit 1; }

# --- a real kitty --------------------------------------------------------
if [ "$NO_KITTY" = 0 ]; then
    step "kitty"
    kitty -o allow_remote_control=yes -o confirm_os_window_close=0 \
          --listen-on "unix:$KITTY_SOCKET" sh -c 'sleep 300' >/dev/null 2>&1 &
    KITTY_PID=$!
    for _ in $(seq 1 20); do [ -S "$KITTY_SOCKET" ] && break; sleep 0.5; done
    [ -S "$KITTY_SOCKET" ] && pass "listening on $KITTY_SOCKET" || fail "kitty never listened"
    export KITTY_LISTEN_ON="unix:$KITTY_SOCKET"
fi

# --- the agent -----------------------------------------------------------
step "agent session"
PROMPT="Do these in order, one tool call each, no subagents: view lines 1-2 of data.txt. \
Create notes.md containing the single line 'checked'. In data.txt, change 'beta' \
to 'BETA'. Run the shell command: printf 'x\\n' > shell.txt. Then call the \
thirdeye flag tool to flag notes.md for me, with a one-line reason. Reply briefly."

( cd "$PROJECT" && env "${UNSET[@]}" timeout 400 agy --dangerously-skip-permissions \
    --model "$MODEL" -p "$PROMPT" < /dev/null ) > "$SCRATCH/agy.log" 2>&1
STATUS=$?
[ "$STATUS" = 0 ] && pass "agy -p completed" \
                  || { fail "agy -p exited $STATUS"; tail -15 "$SCRATCH/agy.log" | sed 's/^/        /'; }
sleep 3

# --- what landed ---------------------------------------------------------
step "the log"
WS_DIR="$(find "$STATE_ROOT" -maxdepth 1 -type d -name 'project-*' -newer "$SCRATCH" 2>/dev/null | head -1)"
SESSION="$(cat "$WS_DIR/active-session" 2>/dev/null)"
SESSION_DIR="$WS_DIR/$SESSION"
[ -n "$SESSION" ] && [ -d "$SESSION_DIR" ] \
    && pass "session $SESSION, the workspace's active one" \
    || { fail "no session recorded for the scratch workspace"; exit 1; }
HOOK="$SESSION_DIR/hook.jsonl"

[ -s "$HOOK" ] && pass "hook.jsonl written" || fail "no hook.jsonl"
OPEN=$(grep '"turn":"open"' "$HOOK" 2>/dev/null | grep -o '"prompt_id":"[^"]*"' | head -1)
[ -n "$OPEN" ] && pass "turn opened by the first model call ($OPEN)" || fail "no turn-open marker"
grep '"turn":"close"' "$HOOK" 2>/dev/null | grep -q "$OPEN" \
    && pass "the same turn closed by the agent stopping" || fail "no matching turn-close marker"

expect() { # $1 file, $2 kind, $3 how it got there
    grep "/$1\"" "$HOOK" 2>/dev/null | grep -q "\"kind\":\"$2\"" \
        && pass "$1 recorded as $2 ($3)" || fail "$1 was not recorded as $2 ($3)"
}
expect data.txt read  "view_file"
expect notes.md write "write_to_file"
expect data.txt edit  "replace_file_content"
expect shell.txt write "observed across the shell call"
grep '"kind":"read"' "$HOOK" 2>/dev/null | grep -q "$OPEN" \
    && pass "tool events joined the held turn" || fail "tool events carry no turn"

grep -q 'alpha' "$HOOK" 2>/dev/null \
    && fail "file content leaked into the log" \
    || pass "no file content in the log"
LONGEST=$(awk '{ if (length($0) > m) m = length($0) } END { print m+0 }' "$HOOK" 2>/dev/null)
[ "${LONGEST:-0}" -le 4096 ] && pass "longest line ${LONGEST}B, within the 4096B cap" \
                             || fail "a line exceeded the atomic-append cap: ${LONGEST}B"

step "attention (depends on the model choosing to flag — reported, not enforced)"
if [ -s "$SESSION_DIR/mcp.jsonl" ]; then
    pass "the flag joined the active session — agy gives MCP servers no id"
    grep -q '"agent":"antigravity"' "$SESSION_DIR/mcp.jsonl" \
        && pass "the flag is labelled antigravity" || fail "the flag is not labelled antigravity"
    grep -q "$OPEN" "$SESSION_DIR/mcp.jsonl" \
        && pass "the flag joined the open turn" || soft "the flag joined no turn"
else
    soft "no flag in this session — elsewhere?"
    find "$STATE_ROOT" -name mcp.jsonl -newer "$SCRATCH" 2>/dev/null | sed 's/^/        /'
fi

step "the daemon"
JOURNAL="$(journalctl --user -u "thirdeye@$SESSION.service" --no-pager -o cat 2>/dev/null)"
if [ "$NO_KITTY" = 0 ]; then
    echo "$JOURNAL" | grep -q "opening the view .* in kitty" \
        && pass "daemon opened the view in kitty" \
        || soft "daemon never opened the view in kitty"
    split_open() {
        kitten @ --to "unix:$KITTY_SOCKET" ls 2>/dev/null \
            | grep -q "\"THIRDEYE_SESSION\": \"$SESSION\""
    }
    # No session-end hook: the daemon polls for agy every 5 s.
    for _ in $(seq 1 40); do split_open || break; sleep 0.5; done
    split_open && fail "the split outlived agy" || pass "the daemon saw agy exit and closed the split"
fi

step "the viewer"
OUT="$(cd "$PROJECT" && thirdeye ls --session "$SESSION" 2>&1)"
grep '"kind":"edit"' "$HOOK" 2>/dev/null | grep -q '"range"' \
    && pass "the edit carries its line range" || fail "the edit carries no range"
echo "$OUT" | grep -q "notes.md" && echo "$OUT" | grep -q "shell.txt" \
    && pass "ls renders the files" || fail "ls did not render the session"
echo "$OUT" | sed 's/^/        /'

step "verdict"
if [ "$FAILURES" = 0 ]; then
    printf '  \033[32mPASS\033[0m — %s soft note(s)\n' "$SOFT"
else
    printf '  \033[31mFAIL\033[0m — %s failure(s), %s soft note(s)\n' "$FAILURES" "$SOFT"
fi
[ "$FAILURES" = 0 ]
