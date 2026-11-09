#!/usr/bin/env bash
# End-to-end smoke test (spec milestone 10).
#
# Drives the whole chain against a real Claude Code session and a real kitty,
# then asserts on what landed. Everything else in this repo is unit-tested;
# this is the part that can only be checked by running it.
#
# Why shell, when `xtask` is Rust: this orchestrates external processes —
# kitty, claude, systemctl — and is meant to be read and run by a human. It is
# also its own gate: it exits non-zero when an assertion fails, which is the
# coverage a lint would otherwise be standing in for.
#
# COSTS MONEY. The agent step runs `claude -p`, which since 2026-06-15 draws
# on the Agent SDK credit pool rather than an interactive subscription.
#
# Usage:  scripts/e2e.sh [--no-kitty]

set -uo pipefail

NO_KITTY=0
[ "${1:-}" = "--no-kitty" ] && NO_KITTY=1

# `claude --session-id` requires a UUID, and it is also what the socket
# name and the unit instance are derived from.
SESSION="$(cat /proc/sys/kernel/random/uuid 2>/dev/null || uuidgen)"
PROJECT="$(mktemp -d /tmp/thirdeye-e2e-XXXXXX)"
KITTY_SOCKET="/tmp/kitty-e2e-$$"
KITTY_PID=""
FAILURES=0
SOFT=0

# The units run under the *user manager's* runtime dir, and the daemon it
# starts inherits none of our environment. Overriding XDG_RUNTIME_DIR or
# THIRDEYE_STATE_DIR here would make the hook and the daemon disagree about
# where the socket and the log live — the trap documented in docs/kitty.md.
# So this test uses the real directories and cleans up after itself.
STATE_ROOT="${XDG_STATE_HOME:-$HOME/.local/state}/thirdeye"

# The agent must reach only the kitty this script launches. Run from inside
# tmux, it would otherwise inherit $TMUX, get a tmux pane (the innermost
# container wins), and split the human's own session while the kitty
# assertions below failed for no reason in the code under test.
unset TMUX TMUX_PANE THIRDEYE_SURFACE KITTY_LISTEN_ON

pass() { printf '  \033[32mok\033[0m    %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }
soft() { printf '  \033[33msoft\033[0m  %s\n' "$1"; SOFT=$((SOFT + 1)); }
step() { printf '\n== %s\n' "$1"; }

cleanup() {
    step "cleaning up"
    [ "$NO_KITTY" = 0 ] && kitten @ --to "unix:$KITTY_SOCKET" close-window \
        --match "env:THIRDEYE_SESSION=$SESSION" >/dev/null 2>&1
    systemctl --user stop "thirdeye@$SESSION.socket" "thirdeye@$SESSION.service" >/dev/null 2>&1
    # By pid, not by pattern: the command line kitty ends up with is not the
    # one we wrote, so a pkill pattern silently matches nothing.
    [ -n "$KITTY_PID" ] && kill "$KITTY_PID" >/dev/null 2>&1
    # Unregistered before the directory goes: Claude Code keeps a local-scope
    # install keyed by the project path, uninstalling needs that directory to
    # exist, and every earlier run left a dead entry behind. `thirdeye
    # uninstall` never runs `marketplace remove`, which is global and took
    # every other project's install with it.
    thirdeye uninstall -a claude "$PROJECT" >/dev/null 2>&1
    rm -rf "$PROJECT" "$KITTY_SOCKET"
    rm -rf "$STATE_ROOT"/*/"$SESSION"
    # -r: the call markers beside the socket are a directory, and outlive a
    # session this script stops before its own end has run.
    rm -rf "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/thirdeye/$SESSION."*
    # The scratch project had its own workspace slug; remove the directory too,
    # or every run leaves one behind for a project that no longer exists. By
    # this project's own name, not by emptiness: `active-session` stays in it.
    rm -rf "$STATE_ROOT/$(basename "$PROJECT")"-*
    echo "  removed the scratch project, session state and units"
}
trap cleanup EXIT

# --- preconditions -------------------------------------------------------
step "preconditions"
command -v thirdeye >/dev/null || { fail "thirdeye is not on PATH — cargo xtask install"; exit 1; }
command -v claude   >/dev/null || { fail "claude is not on PATH"; exit 1; }
pass "thirdeye: $(thirdeye --version)"

if ! thirdeye doctor --user | grep -q "Everything checkable agrees"; then
    thirdeye doctor --user
    fail "doctor reports drift — fix that first, or this test measures the wrong build"
    exit 1
fi
pass "doctor: binary, plugin and units agree"

if [ "$NO_KITTY" = 0 ]; then
    command -v kitty >/dev/null || { fail "kitty is not on PATH (use --no-kitty)"; exit 1; }
fi

# --- a scratch project ---------------------------------------------------
step "scratch project at $PROJECT"
cat > "$PROJECT/notes.md" <<'EOF'
# Notes

One line here.
EOF
printf 'alpha\nbeta\ngamma\n' > "$PROJECT/data.txt"
thirdeye install -a claude "$PROJECT" >/dev/null 2>&1
grep -q thirdeye "$PROJECT/.claude/settings.local.json" 2>/dev/null \
    && pass "plugin wired into the scratch project" \
    || fail "plugin was not wired"

# --- a real kitty --------------------------------------------------------
if [ "$NO_KITTY" = 0 ]; then
    step "kitty"
    kitty -o allow_remote_control=yes -o confirm_os_window_close=0 \
          --listen-on "unix:$KITTY_SOCKET" sh -c 'sleep 120' >/dev/null 2>&1 &
    KITTY_PID=$!
    for _ in $(seq 1 20); do [ -S "$KITTY_SOCKET" ] && break; sleep 0.5; done
    [ -S "$KITTY_SOCKET" ] && pass "listening on $KITTY_SOCKET" || fail "kitty never listened"
    export KITTY_LISTEN_ON="unix:$KITTY_SOCKET"
fi

# --- the agent -----------------------------------------------------------
step "agent session (this is the part that costs credit)"
PROMPT="Read data.txt. Then append a line saying 'checked' to notes.md. \
Then, using a shell command rather than your file-editing tools, append a line \
saying 'delta' to data.txt. \
Then run exactly this shell command, which is meant to fail: \
printf 'x\\n' > failed.txt && false \
Finally tell me which of the two files I should look at myself, and why."

( cd "$PROJECT" && timeout 300 claude -p "$PROMPT" \
    --session-id "$SESSION" \
    --allowedTools Read Edit Write Bash \
      "mcp__plugin_thirdeye_thirdeye__flag" "mcp__thirdeye__flag" \
    >/dev/null 2>&1 )
pass "session $SESSION completed"
sleep 2

# --- what landed ---------------------------------------------------------
step "the log"
SESSION_DIR="$(find "$STATE_ROOT" -type d -name "$SESSION" 2>/dev/null | head -1)"
[ -n "$SESSION_DIR" ] && pass "session directory: ${SESSION_DIR/#$HOME/\~}" \
                      || fail "no session directory under $STATE_ROOT"

[ -s "$SESSION_DIR/hook.jsonl" ] && pass "hook.jsonl written" || fail "no hook.jsonl"
grep -q '"turn":"open"'  "$SESSION_DIR/hook.jsonl" 2>/dev/null \
    && pass "turn opened by UserPromptSubmit" || fail "no turn-open marker"
grep -q '"turn":"close"' "$SESSION_DIR/hook.jsonl" 2>/dev/null \
    && pass "turn closed by Stop"            || fail "no turn-close marker"
grep -q '"kind":"read"'  "$SESSION_DIR/hook.jsonl" 2>/dev/null \
    && pass "a read was recorded"            || fail "no read recorded"
grep -qE '"kind":"(edit|write)"' "$SESSION_DIR/hook.jsonl" 2>/dev/null \
    && pass "a mutation was recorded"        || fail "no mutation recorded"
# Only the shell observer can put data.txt in the log as a write: Claude
# changed it with a command, which no structured tool reports.
grep 'data.txt' "$SESSION_DIR/hook.jsonl" 2>/dev/null | grep -q '"kind":"write"' \
    && pass "a write made through the shell was observed" \
    || fail "the shell's write to data.txt was not observed"
# A command that exits non-zero fires PostToolUseFailure, not PostToolUse;
# what it wrote before failing must still be observed.
grep 'failed.txt' "$SESSION_DIR/hook.jsonl" 2>/dev/null | grep -q '"kind":"write"' \
    && pass "a write made by a failing command was observed" \
    || fail "the failing command's write to failed.txt was not observed"

# Content must never reach the log: tool_response embeds whole files.
grep -q 'alpha' "$SESSION_DIR/hook.jsonl" 2>/dev/null \
    && fail "file CONTENT leaked into the log" || pass "no file content in the log"

LONGEST=$(awk '{ if (length > m) m = length } END { print m+0 }' "$SESSION_DIR/hook.jsonl" 2>/dev/null)
[ "${LONGEST:-0}" -le 4096 ] && pass "longest line ${LONGEST}B, within the 4096B cap" \
                             || fail "a line exceeded the atomic-append cap: ${LONGEST}B"

step "the daemon"
systemctl --user is-active "thirdeye@$SESSION.service" >/dev/null 2>&1 \
    && pass "daemon was socket-activated" \
    || soft "daemon not active now (it may have been stopped by SessionEnd)"
# The journal names the surface, so this also proves the kitty surface was
# the one chosen — the window itself is gone by now, taken by SessionEnd.
JOURNAL="$(journalctl --user -u "thirdeye@$SESSION.service" --no-pager -o cat 2>/dev/null)"
if [ "$NO_KITTY" = 0 ]; then
    echo "$JOURNAL" | grep -q "opening the view .* in kitty" \
        && pass "daemon opened the view in kitty" \
        || soft "daemon never opened the view in kitty (no mutation reached it?)"
else
    echo "$JOURNAL" | grep -q "opening the view" \
        && pass "daemon decided to open the view" \
        || soft "daemon never opened the view (no mutation reached it?)"
fi

step "attention (depends on the model choosing to flag — reported, not enforced)"
if [ -s "$SESSION_DIR/mcp.jsonl" ]; then
    pass "mcp.jsonl written — the skill fired and the tool was called"
    grep -q '"kind":"flag"' "$SESSION_DIR/mcp.jsonl" && pass "a flag event was recorded"
    FLAG_TURN=$(grep -o '"prompt_id":"[^"]*"' "$SESSION_DIR/mcp.jsonl" | head -1)
    [ -n "$FLAG_TURN" ] && pass "the flag joined its turn ($FLAG_TURN)" \
                        || soft "the flag has no prompt_id — it will render ungrouped"
else
    soft "no flag this run; the agent answered in prose. Not a code failure:
        whether to flag is the model's judgement, which is what the skill exists
        to influence and what no assertion here can guarantee."
fi

step "the viewer"
if [ "$NO_KITTY" = 0 ]; then
    # The split should be *gone* by now: SessionEnd stops the daemon and the
    # head exits with it, closing its window. Polled, because the SessionEnd
    # hook is async and may still be running.
    split_open() {
        kitten @ --to "unix:$KITTY_SOCKET" ls 2>/dev/null \
            | grep -q "\"THIRDEYE_SESSION\": \"$SESSION\""
    }
    for _ in $(seq 1 20); do split_open || break; sleep 0.5; done
    if ! split_open; then
        pass "no split outlived the session"
    elif systemctl --user is-active "thirdeye@$SESSION.service" >/dev/null 2>&1; then
        soft "the split is still open, but so is the session's daemon (SessionEnd not run yet?)"
    else
        fail "the split outlived its session — its head did not exit with the daemon"
    fi
fi
OUT="$(thirdeye ls "$PROJECT" --session "$SESSION" 2>&1)"
echo "$OUT" | grep -q "notes.md" && pass "ls renders the touched files" \
                                 || fail "ls did not render the session"
echo "$OUT" | sed 's/^/        /'

# --- verdict -------------------------------------------------------------
step "verdict"
if [ "$FAILURES" -eq 0 ]; then
    printf '  \033[32mPASS\033[0m — %d soft note(s)\n' "$SOFT"
    exit 0
fi
printf '  \033[31m%d FAILURE(S)\033[0m, %d soft note(s)\n' "$FAILURES" "$SOFT"
exit 1
