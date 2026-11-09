#!/usr/bin/env bash
# End-to-end smoke test for pi — the sibling of scripts/e2e.sh,
# e2e-codex.sh and e2e-opencode.sh.
#
# Drives a real `pi -p` against a project wired by `thirdeye install pi`: the
# extension forwarding prompts, tools and turn ends; reads, writes and edits
# translated; a shell write observed; a flag through the extension's own
# tool; the daemon and a kitty split. Facts: docs/pi-extension-events.md.
#
# --approve trusts the scratch project's .pi/ for this run only, as a human
# trusting the project would.
#
# COSTS MONEY: one short task on $THIRDEYE_PI_MODEL if set (e.g.
# openai-codex/gpt-6-luna), else pi's own default model.
#
# Usage:  scripts/e2e-pi.sh [--no-kitty]

set -uo pipefail

NO_KITTY=0
[ "${1:-}" = "--no-kitty" ] && NO_KITTY=1
MODEL="${THIRDEYE_PI_MODEL:-}"

CACHE="${XDG_CACHE_HOME:-$HOME/.cache}"
mkdir -p "$CACHE"
SCRATCH="$(mktemp -d "$CACHE/thirdeye-e2e-pi-XXXXXX")"
chmod 700 "$SCRATCH"
PROJECT="$SCRATCH/project"
KITTY_SOCKET="/tmp/kitty-e2e-pi-$$"
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
    cp "$SCRATCH/pi.log" "$CACHE/thirdeye-e2e-pi-last.log" 2>/dev/null \
        && echo "  pi's log: ${CACHE/#$HOME/\~}/thirdeye-e2e-pi-last.log"
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
command -v pi >/dev/null || { fail "pi is not on PATH"; exit 1; }
pass "thirdeye: $(thirdeye --version)"
pass "pi: $(pi --version 2>/dev/null), model ${MODEL:-its default}"
if [ "$NO_KITTY" = 0 ]; then
    command -v kitty >/dev/null || { fail "kitty is not on PATH (use --no-kitty)"; exit 1; }
fi

# --- a scratch project, wired --------------------------------------------
step "scratch project at ${PROJECT/#$HOME/\~}"
mkdir -p "$PROJECT"
printf 'alpha\nbeta\ngamma\n' > "$PROJECT/data.txt"
git -C "$PROJECT" init -q
# Tracked, so its deletion is seen even though the agent never touched it.
echo "obsolete" > "$PROJECT/old.txt"
git -C "$PROJECT" add old.txt
thirdeye install -a pi "$PROJECT" >/dev/null \
    && [ -f "$PROJECT/.pi/extensions/thirdeye/index.js" ] \
    && pass "thirdeye install pi wrote the extension" \
    || fail "thirdeye install pi failed"

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
tool to flag notes.md for me, with a one-line reason. Finally run the shell \
command: rm old.txt. Reply briefly."

( cd "$PROJECT" && env "${UNSET[@]}" timeout 400 pi -p --approve \
    ${MODEL:+--model "$MODEL"} "$PROMPT" < /dev/null ) > "$SCRATCH/pi.log" 2>&1
STATUS=$?
[ "$STATUS" = 0 ] && pass "pi completed" \
                  || { fail "pi exited $STATUS"; tail -15 "$SCRATCH/pi.log" | sed 's/^/        /'; }
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
    && pass "turn closed when the agent settled" || fail "no matching turn-close marker"

expect() { # $1 file, $2 kind, $3 how it got there
    grep "/$1\"" "$HOOK" 2>/dev/null | grep -q "\"kind\":\"$2\"" \
        && pass "$1 recorded as $2 ($3)" || fail "$1 was not recorded as $2 ($3)"
}
expect data.txt read  "the read tool"
expect notes.md write "the write tool"
expect data.txt edit  "the edit tool"
expect shell.txt write "observed across the bash call"
expect old.txt delete "observed across the bash call"
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
    pass "the flag landed in the session — the extension passed its exact id"
    grep -q '"agent":"pi"' "$SESSION_DIR/mcp.jsonl" \
        && pass "the flag is labelled pi" || fail "the flag is not labelled pi"
else
    soft "no flag this run — what pi said about it:"
    grep -iE 'thirdeye_flag|flag' "$SCRATCH/pi.log" | head -5 | sed 's/^/        /'
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
