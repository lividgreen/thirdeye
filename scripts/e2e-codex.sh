#!/usr/bin/env bash
# End-to-end smoke test for Codex CLI — scripts/e2e.sh's sibling.
#
# Drives the whole chain against a real `codex exec` and a real kitty: hooks
# written by `thirdeye install codex`, the shell writes found by watching the
# workspace, a flag attributed through the workspace's active session, the
# daemon, the split. Payload facts it relies on: docs/codex-hook-payload.md.
#
# Isolated like scripts/codex-capture.sh: a throwaway CODEX_HOME with your
# auth.json linked in, so your own Codex config, hooks and trust records are
# never touched. Hooks run under --dangerously-bypass-hook-trust, because
# `codex exec` cannot show the /hooks approval a human would give.
#
# COSTS MONEY: one short `codex exec` task on your OpenAI plan, on
# $THIRDEYE_CODEX_MODEL if set (e.g. gpt-6-luna), else Codex's default.
#
# Usage:  scripts/e2e-codex.sh [--no-kitty]

set -uo pipefail

NO_KITTY=0
[ "${1:-}" = "--no-kitty" ] && NO_KITTY=1

REAL_CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
MODEL="${THIRDEYE_CODEX_MODEL:-}"
# Not /tmp for CODEX_HOME: Codex refuses to install its helper binaries
# under a temporary directory.
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}"
mkdir -p "$CACHE"
SCRATCH="$(mktemp -d "$CACHE/thirdeye-e2e-codex-XXXXXX")"
chmod 700 "$SCRATCH"
PROJECT="$SCRATCH/project"
export CODEX_HOME="$SCRATCH/codex-home"
KITTY_SOCKET="/tmp/kitty-e2e-codex-$$"
KITTY_PID=""
FAILURES=0
SOFT=0
STATE_ROOT="${XDG_STATE_HOME:-$HOME/.local/state}/thirdeye"

# The agent must reach only the kitty this script launches (see e2e.sh).
unset TMUX TMUX_PANE THIRDEYE_SURFACE KITTY_LISTEN_ON
# Run from inside Claude Code, its variables leak into Codex's hooks and MCP
# server and read as Codex's own — the first capture fell for exactly that.
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
    # Kept: the one artifact worth reading after a surprising run. It holds
    # the agent's transcript, so it stays owner-only under ~/.cache.
    cp "$SCRATCH/codex.log" "$CACHE/thirdeye-e2e-codex-last.log" 2>/dev/null \
        && echo "  Codex's log: ${CACHE/#$HOME/\~}/thirdeye-e2e-codex-last.log"
    rm -rf "$SCRATCH" "$KITTY_SOCKET"
    # The scratch project's workspace directory: its sessions are gone, and
    # active-session alone does not make it worth keeping.
    for ws in "$STATE_ROOT"/project-*/; do
        [ -d "$ws" ] || continue
        [ "$(find "$ws" -mindepth 1 -maxdepth 1 -type d | wc -l)" = 0 ] && rm -rf "$ws"
    done
    echo "  removed the scratch project, Codex home, session state and units"
}
trap cleanup EXIT

# --- preconditions -------------------------------------------------------
step "preconditions"
command -v thirdeye >/dev/null || { fail "thirdeye is not on PATH — cargo xtask install"; exit 1; }
command -v codex    >/dev/null || { fail "codex is not on PATH (npm i -g @openai/codex)"; exit 1; }
if [ ! -e "$REAL_CODEX_HOME/auth.json" ] && [ -z "${OPENAI_API_KEY:-}" ]; then
    fail "not signed in to Codex: run \`codex login\`"; exit 1
fi
pass "thirdeye: $(thirdeye --version)"
pass "codex: $(codex --version 2>/dev/null), model ${MODEL:-its default}"
if [ "$NO_KITTY" = 0 ]; then
    command -v kitty >/dev/null || { fail "kitty is not on PATH (use --no-kitty)"; exit 1; }
fi

# --- a scratch project, wired --------------------------------------------
step "scratch project at ${PROJECT/#$HOME/\~}"
mkdir -p "$PROJECT" "$CODEX_HOME"
[ -e "$REAL_CODEX_HOME/auth.json" ] && ln -s "$REAL_CODEX_HOME/auth.json" "$CODEX_HOME/auth.json"
printf 'alpha\nbeta\ngamma\n' > "$PROJECT/data.txt"
git -C "$PROJECT" init -q
# A project's .codex/ layer loads only once the project is trusted.
printf '[projects."%s"]\ntrust_level = "trusted"\n' "$PROJECT" > "$CODEX_HOME/config.toml"

thirdeye install -a codex "$PROJECT" >/dev/null \
    && pass "thirdeye install codex wired the project" \
    || fail "thirdeye install codex failed"
grep -q 'hook --agent codex' "$PROJECT/.codex/hooks.json" 2>/dev/null \
    && pass "hooks.json runs thirdeye" || fail "no thirdeye hook in .codex/hooks.json"

# --- a real kitty --------------------------------------------------------
if [ "$NO_KITTY" = 0 ]; then
    step "kitty"
    kitty -o allow_remote_control=yes -o confirm_os_window_close=0 \
          --listen-on "unix:$KITTY_SOCKET" sh -c 'sleep 180' >/dev/null 2>&1 &
    KITTY_PID=$!
    for _ in $(seq 1 20); do [ -S "$KITTY_SOCKET" ] && break; sleep 0.5; done
    [ -S "$KITTY_SOCKET" ] && pass "listening on $KITTY_SOCKET" || fail "kitty never listened"
    export KITTY_LISTEN_ON="unix:$KITTY_SOCKET"
fi

# --- the agent -----------------------------------------------------------
step "agent session (this is the part that costs money)"
PROMPT="Read data.txt and tell me its second line. Create notes.md containing the \
single line 'checked'. In data.txt, change 'beta' to 'BETA'. Then use the thirdeye \
flag tool to flag notes.md for me, with a one-line reason. Reply briefly."

( cd "$PROJECT" && env "${UNSET[@]}" timeout 400 codex exec \
    --dangerously-bypass-hook-trust \
    --sandbox workspace-write \
    ${MODEL:+--model "$MODEL"} \
    "$PROMPT" < /dev/null ) > "$SCRATCH/codex.log" 2>&1
STATUS=$?
[ "$STATUS" = 0 ] && pass "codex exec completed" \
                  || { fail "codex exec exited $STATUS"; tail -15 "$SCRATCH/codex.log" | sed 's/^/        /'; }
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
[ -n "$OPEN" ] && pass "turn opened by UserPromptSubmit" || fail "no turn-open marker"
grep '"turn":"close"' "$HOOK" 2>/dev/null | grep -q "$OPEN" \
    && pass "turn closed by Stop, same turn_id" || fail "no matching turn-close marker"

for f in notes.md data.txt; do
    grep "/$f\"" "$HOOK" 2>/dev/null | grep -q '"kind":"write"' \
        && pass "$f observed as written" \
        || fail "$f was changed but not observed"
done
# Codex reads only through its shell; the forms it was captured using
# (cat, sed -n) are recognized. Which command it picks is the model's call.
grep '/data.txt"' "$HOOK" 2>/dev/null | grep -q '"kind":"read"' \
    && pass "data.txt recorded as read, from the shell command" \
    || soft "no read of data.txt recognized — see the commands in Codex's log"
grep '"kind":"write"' "$HOOK" 2>/dev/null | grep -q "$OPEN" \
    && pass "the writes joined their turn" || fail "observed writes carry no turn"

# Command output must never reach the log: for `cat`, it is the file.
grep -q 'alpha' "$HOOK" 2>/dev/null \
    && fail "file content leaked into the log" \
    || pass "no command output in the log"
LONGEST=$(awk '{ if (length($0) > m) m = length($0) } END { print m+0 }' "$HOOK" 2>/dev/null)
[ "${LONGEST:-0}" -le 4096 ] && pass "longest line ${LONGEST}B, within the 4096B cap" \
                             || fail "a line exceeded the atomic-append cap: ${LONGEST}B"

step "attention (depends on the model choosing to flag — reported, not enforced)"
if [ -s "$SESSION_DIR/mcp.jsonl" ]; then
    pass "the flag landed in the session — attributed through the active pointer"
    grep -q '"agent":"codex"' "$SESSION_DIR/mcp.jsonl" \
        && pass "the flag is labelled codex" || fail "the flag is not labelled codex"
elif ls "$WS_DIR/_unattributed/mcp.jsonl" >/dev/null 2>&1; then
    fail "a flag was raised but landed in _unattributed — the active-session fallback failed"
else
    soft "no flag this run — what Codex said about MCP and the flag:"
    grep -iE 'mcp|tool|flag|approv|elicit|error|fail' "$SCRATCH/codex.log" \
        | grep -v -- '^[a-z]' | head -15 | sed 's/^/        /'
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
echo "$OUT" | grep -q "notes.md" && echo "$OUT" | grep -q "data.txt" \
    && pass "ls renders both files" || fail "ls did not render the session"
echo "$OUT" | sed 's/^/        /'

step "verdict"
if [ "$FAILURES" = 0 ]; then
    printf '  \033[32mPASS\033[0m — %s soft note(s)\n' "$SOFT"
else
    printf '  \033[31mFAIL\033[0m — %s failure(s), %s soft note(s)\n' "$FAILURES" "$SOFT"
fi
[ "$FAILURES" = 0 ]
