#!/usr/bin/env bash
# End-to-end smoke test for OpenCode — the sibling of scripts/e2e.sh and
# scripts/e2e-codex.sh.
#
# Drives a real `opencode run --standalone` against a project wired by
# `thirdeye install opencode`: the plugin forwarding prompts, tools and turn
# ends; reads, writes and edits translated; a shell write observed; a flag
# through the plugin's own tool; the daemon and a kitty split. Facts it relies
# on: docs/opencode-plugin-events.md.
#
# --standalone, because a standalone server inherits this terminal's
# environment — KITTY_LISTEN_ON included. The shared background service does
# not, which is the known gap docs/install.md describes.
#
# The model is $THIRDEYE_OPENCODE_MODEL, default a free one. It may still cost
# money with a paid model.
#
# Usage:  scripts/e2e-opencode.sh [--no-kitty]

set -uo pipefail

NO_KITTY=0
[ "${1:-}" = "--no-kitty" ] && NO_KITTY=1
MODEL="${THIRDEYE_OPENCODE_MODEL:-opencode/mimo-v2.6-flash-free}"

CACHE="${XDG_CACHE_HOME:-$HOME/.cache}"
mkdir -p "$CACHE"
SCRATCH="$(mktemp -d "$CACHE/thirdeye-e2e-opencode-XXXXXX")"
chmod 700 "$SCRATCH"
PROJECT="$SCRATCH/project"
KITTY_SOCKET="/tmp/kitty-e2e-opencode-$$"
KITTY_PID=""
FAILURES=0
SOFT=0
STATE_ROOT="${XDG_STATE_HOME:-$HOME/.local/state}/thirdeye"

# The agent must reach only the kitty this script launches (see e2e.sh).
unset TMUX TMUX_PANE THIRDEYE_SURFACE KITTY_LISTEN_ON
UNSET=()
for v in $(compgen -e | grep -E '^CLAUDE'); do UNSET+=(-u "$v"); done

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
    cp "$SCRATCH/opencode.log" "$CACHE/thirdeye-e2e-opencode-last.log" 2>/dev/null \
        && echo "  OpenCode's log: ${CACHE/#$HOME/\~}/thirdeye-e2e-opencode-last.log"
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
command -v opencode >/dev/null || { fail "opencode is not on PATH"; exit 1; }
pass "thirdeye: $(thirdeye --version)"
pass "opencode: $(opencode --version 2>/dev/null), model $MODEL"
if [ "$NO_KITTY" = 0 ]; then
    command -v kitty >/dev/null || { fail "kitty is not on PATH (use --no-kitty)"; exit 1; }
fi

# --- a scratch project, wired --------------------------------------------
step "scratch project at ${PROJECT/#$HOME/\~}"
mkdir -p "$PROJECT"
printf 'alpha\nbeta\ngamma\n' > "$PROJECT/data.txt"
git -C "$PROJECT" init -q
thirdeye install -a opencode "$PROJECT" >/dev/null \
    && [ -f "$PROJECT/.opencode/plugins/thirdeye/index.js" ] \
    && pass "thirdeye install opencode wrote the plugin" \
    || fail "thirdeye install opencode failed"

# --- a real kitty --------------------------------------------------------
if [ "$NO_KITTY" = 0 ]; then
    step "kitty"
    kitty -o allow_remote_control=yes -o confirm_os_window_close=0 \
          --listen-on "unix:$KITTY_SOCKET" sh -c 'sleep 240' >/dev/null 2>&1 &
    KITTY_PID=$!
    for _ in $(seq 1 20); do [ -S "$KITTY_SOCKET" ] && break; sleep 0.5; done
    [ -S "$KITTY_SOCKET" ] && pass "listening on $KITTY_SOCKET" || fail "kitty never listened"
    export KITTY_LISTEN_ON="unix:$KITTY_SOCKET"
fi

# --- the agent -----------------------------------------------------------
step "agent session"
PROMPT="Read data.txt and tell me its second line. Create notes.md containing the \
single line 'checked'. In data.txt, change 'beta' to 'BETA' using your edit tool. \
Then run the shell command: printf 'x\\n' > shell.txt. Then call the thirdeye_flag \
tool to flag notes.md for me, with a one-line reason. Reply briefly."

( cd "$PROJECT" && env "${UNSET[@]}" timeout 400 opencode run --standalone --auto \
    -m "$MODEL" "$PROMPT" < /dev/null ) > "$SCRATCH/opencode.log" 2>&1
STATUS=$?
[ "$STATUS" = 0 ] && pass "opencode run completed" \
                  || { fail "opencode run exited $STATUS"; tail -15 "$SCRATCH/opencode.log" | sed 's/^/        /'; }
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
[ -n "$OPEN" ] && pass "turn opened by the prompt" || fail "no turn-open marker"
grep '"turn":"close"' "$HOOK" 2>/dev/null | grep -q "$OPEN" \
    && pass "turn closed by the execution ending" || fail "no matching turn-close marker"

expect() { # $1 file, $2 kind, $3 how it got there
    grep "/$1\"" "$HOOK" 2>/dev/null | grep -q "\"kind\":\"$2\"" \
        && pass "$1 recorded as $2 ($3)" || fail "$1 was not recorded as $2 ($3)"
}
expect data.txt read  "the read tool"
expect notes.md write "the write tool"
expect data.txt edit  "the edit tool"
expect shell.txt write "observed across the shell call"
grep '"kind":"edit"' "$HOOK" 2>/dev/null | grep -q '"range"' \
    && pass "the edit carries its line range" || soft "the edit carries no range"
grep '"kind":"read"' "$HOOK" 2>/dev/null | grep -q "$OPEN" \
    && pass "tool events joined their turn" || fail "tool events carry no turn"

grep -q 'alpha' "$HOOK" 2>/dev/null \
    && fail "file content leaked into the log" \
    || pass "no file content in the log"
LONGEST=$(awk '{ if (length($0) > m) m = length($0) } END { print m+0 }' "$HOOK" 2>/dev/null)
[ "${LONGEST:-0}" -le 4096 ] && pass "longest line ${LONGEST}B, within the 4096B cap" \
                             || fail "a line exceeded the atomic-append cap: ${LONGEST}B"

step "attention (depends on the model choosing to flag — reported, not enforced)"
if [ -s "$SESSION_DIR/mcp.jsonl" ]; then
    pass "the flag landed in the session — the plugin passed its exact id"
    grep -q '"agent":"opencode"' "$SESSION_DIR/mcp.jsonl" \
        && pass "the flag is labelled opencode" || fail "the flag is not labelled opencode"
else
    soft "no flag this run — what OpenCode said about it:"
    grep -iE 'thirdeye_flag|flag' "$SCRATCH/opencode.log" | head -5 | sed 's/^/        /'
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
