#!/usr/bin/env bash
# End-to-end smoke test for Grok Build — the sibling of scripts/e2e.sh and
# scripts/e2e-copilot.sh.
#
# Drives a real `grok -p` with thirdeye's plugin: a turn held between the
# prompt and its Stop, since Grok's tool events name none; a read, a write,
# an edit with its range; a shell write observed; a flag through the plugin's
# MCP server, in its own session; the daemon and a kitty split. Facts it
# relies on: docs/grok-hook-payload.md.
#
# Grok installs plugins per user. If thirdeye's is not installed already,
# this installs it and removes it again at the end; if it is, it is used as
# it stands — reinstall it first to test this build's plugin.
#
# The model is $THIRDEYE_GROK_MODEL, default Grok's own. To run on another
# provider, point $GROK_HOME at a directory whose config.toml defines a
# custom model and name it here; the plugin is then installed into that home.
#
# Usage:  scripts/e2e-grok.sh [--no-kitty]

set -uo pipefail

NO_KITTY=0
[ "${1:-}" = "--no-kitty" ] && NO_KITTY=1

MODEL="${THIRDEYE_GROK_MODEL:-}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}"
mkdir -p "$CACHE"
SCRATCH="$(mktemp -d "$CACHE/thirdeye-e2e-grok-XXXXXX")"
chmod 700 "$SCRATCH"
PROJECT="$SCRATCH/project"
KITTY_SOCKET="/tmp/kitty-e2e-grok-$$"
KITTY_PID=""
FAILURES=0
SOFT=0
STATE_ROOT="${XDG_STATE_HOME:-$HOME/.local/state}/thirdeye"
INSTALLED_HERE=0

# The agent must reach only the kitty this script launches (see e2e.sh), and
# none of an enclosing agent's session.
unset TMUX TMUX_PANE THIRDEYE_SURFACE KITTY_LISTEN_ON
UNSET=()
for v in $(compgen -e | grep -E '^(CLAUDE|COPILOT_AGENT|ANTIGRAVITY|GROK_SESSION|GROK_HOOK)'); do UNSET+=(-u "$v"); done

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
    if [ "$INSTALLED_HERE" = 1 ]; then
        thirdeye uninstall --user -a grok >/dev/null 2>&1 \
            && echo "  removed the plugin this run installed"
    fi
    cp "$SCRATCH/grok.log" "$CACHE/thirdeye-e2e-grok-last.log" 2>/dev/null \
        && echo "  Grok's output: ${CACHE/#$HOME/\~}/thirdeye-e2e-grok-last.log"
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
command -v grok >/dev/null || { fail "grok is not on PATH"; exit 1; }
pass "thirdeye: $(thirdeye --version)"
pass "grok: $(grok --version 2>/dev/null | head -1), model ${MODEL:-its default}"
if [ "$NO_KITTY" = 0 ]; then
    command -v kitty >/dev/null || { fail "kitty is not on PATH (use --no-kitty)"; exit 1; }
fi

# --- the plugin ----------------------------------------------------------
step "plugin"
if grok plugin list 2>/dev/null | grep -q 'thirdeye/grok/thirdeye\]'; then
    pass "thirdeye's plugin already installed — used as it stands"
else
    thirdeye install --user -a grok >/dev/null 2>&1 \
        && INSTALLED_HERE=1 \
        && pass "thirdeye install --user -a grok had Grok install the plugin" \
        || { fail "thirdeye install --user -a grok failed"; exit 1; }
fi

# --- a scratch project ---------------------------------------------------
step "scratch project at ${PROJECT/#$HOME/\~}"
mkdir -p "$PROJECT"
printf 'alpha\nbeta\ngamma\n' > "$PROJECT/data.txt"
git -C "$PROJECT" init -q && pass "a git repository, so observation is bounded by git"

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
PROMPT="Do these in order, one tool call each, no subagents: read data.txt. \
Create notes.md containing the single line 'checked'. In data.txt, change 'beta' \
to 'BETA'. Run the shell command: printf 'x\\n' > shell.txt. Then call the \
thirdeye__flag tool (an MCP tool; find it with your tool search, and do not use \
the shell to flag) to flag notes.md for me, with a one-line reason. Reply briefly."

MODEL_ARGS=()
[ -n "$MODEL" ] && MODEL_ARGS=(-m "$MODEL")
( cd "$PROJECT" && env "${UNSET[@]}" timeout 400 grok --yolo "${MODEL_ARGS[@]}" \
    -p "$PROMPT" < /dev/null ) > "$SCRATCH/grok.log" 2>&1
STATUS=$?
[ "$STATUS" = 0 ] && pass "grok -p completed" \
                  || { fail "grok -p exited $STATUS"; tail -15 "$SCRATCH/grok.log" | sed 's/^/        /'; }
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
[ -n "$OPEN" ] && pass "turn opened by the prompt, under Grok's own id ($OPEN)" || fail "no turn-open marker"
grep '"turn":"close"' "$HOOK" 2>/dev/null | grep -q "$OPEN" \
    && pass "the same turn closed by the agent stopping" || fail "no matching turn-close marker"

expect() { # $1 file, $2 kind, $3 how it got there
    grep "/$1\"" "$HOOK" 2>/dev/null | grep -q "\"kind\":\"$2\"" \
        && pass "$1 recorded as $2 ($3)" || fail "$1 was not recorded as $2 ($3)"
}
expect data.txt read  "read_file"
expect notes.md write "write"
expect data.txt edit  "search_replace"
grep '"kind":"edit"' "$HOOK" 2>/dev/null | grep -q '"range"' \
    && pass "the edit carries its line range" || fail "the edit carries no range"
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
    pass "the flag landed in its own session — Grok tells MCP servers which"
    grep -q '"agent":"grok"' "$SESSION_DIR/mcp.jsonl" \
        && pass "the flag is labelled grok" || fail "the flag is not labelled grok"
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
    for _ in $(seq 1 20); do split_open || break; sleep 0.5; done
    split_open && fail "the split outlived its session" || pass "no split outlived the session"
fi

step "the viewer"
OUT="$(cd "$PROJECT" && thirdeye ls --session "$SESSION" 2>&1)"
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
