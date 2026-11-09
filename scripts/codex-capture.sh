#!/usr/bin/env bash
# Capture real Codex hook payloads — the spike that gates a Codex adapter.
#
# The project rule is that payload shapes are captured from a live agent,
# never inferred (docs/hook-payload.md). docs/codex-hooks.md lists what the
# published schemas claim and the questions only a real run can answer; this
# produces the evidence.
#
# Isolated on purpose: a throwaway CODEX_HOME with your auth.json linked in,
# so your own config, hooks and hook-trust records are never read or changed.
# Raw captures stay under ~/.cache, uncommitted, because they contain file
# contents. Not /tmp: Codex refuses to create its PATH helper binaries under
# a temporary directory, and a capture missing them may not show the tool
# calls a real install makes.
#
# COSTS MONEY: one short `codex exec` task on your OpenAI plan.
#
# Usage:  scripts/codex-capture.sh ["prompt"]

set -uo pipefail

command -v codex >/dev/null || { echo "codex is not on PATH (npm i -g @openai/codex)"; exit 1; }
REAL_HOME="${CODEX_HOME:-$HOME/.codex}"
if [ ! -e "$REAL_HOME/auth.json" ] && [ -z "${OPENAI_API_KEY:-}" ]; then
    echo "not signed in: run \`codex login\` first (no $REAL_HOME/auth.json, no OPENAI_API_KEY)"
    exit 1
fi

CACHE="${XDG_CACHE_HOME:-$HOME/.cache}"
mkdir -p "$CACHE"
OUT="$(mktemp -d "$CACHE/thirdeye-codex-capture-XXXXXX")"
chmod 700 "$OUT"
PROJECT="$OUT/project"
CAPTURES="$OUT/captures"
export CODEX_HOME="$OUT/codex-home"
mkdir -p "$PROJECT" "$CAPTURES" "$CODEX_HOME"
[ -e "$REAL_HOME/auth.json" ] && ln -s "$REAL_HOME/auth.json" "$CODEX_HOME/auth.json"

printf 'alpha\nbeta\ngamma\n' > "$PROJECT/data.txt"

# The dump hook: stdin verbatim, plus the environment variables that could
# matter to thirdeye (surfaces and session ids). Prints nothing, exactly like
# thirdeye's real hook — so Stop's "expects JSON on stdout" is tested as-is.
cat > "$OUT/dump.sh" <<EOF
#!/usr/bin/env bash
stamp="\$(date +%s%N)-\$\$"
payload="\$(cat)"
event="\$(printf '%s' "\$payload" | sed -n 's/.*"hook_event_name" *: *"\([A-Za-z]*\)".*/\1/p' | head -1)"
printf '%s\n' "\$payload" > "$CAPTURES/\${event:-unknown}-\$stamp.json"
env | grep -E '^(CODEX|CLAUDE|PLUGIN|TMUX|KITTY|TERM|.*SESSION|.*TURN)' | sort > "$CAPTURES/\${event:-unknown}-\$stamp.env"
exit 0
EOF
chmod +x "$OUT/dump.sh"

cat > "$CODEX_HOME/hooks.json" <<EOF
{
  "hooks": {
    "SessionStart":     [{ "hooks": [{ "type": "command", "command": "$OUT/dump.sh" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "$OUT/dump.sh" }] }],
    "PostToolUse":      [{ "matcher": "*", "hooks": [{ "type": "command", "command": "$OUT/dump.sh" }] }],
    "Stop":             [{ "hooks": [{ "type": "command", "command": "$OUT/dump.sh" }] }],
    "SessionEnd":       [{ "hooks": [{ "type": "command", "command": "$OUT/dump.sh" }] }]
  }
}
EOF

# An MCP "server" that records its environment and then never answers: the
# only question is whether Codex hands a session id to MCP servers, which
# thirdeye's flag tool depends on. Codex will report it failed to start.
cat > "$CODEX_HOME/config.toml" <<EOF
[mcp_servers.envprobe]
command = "sh"
args = ["-c", "env | sort > '$CAPTURES/mcp-server.env'; cat > /dev/null"]
startup_timeout_sec = 3
EOF

# Neutral by default: naming shell commands in the prompt steers the model to
# make its edits through the shell too, which hides how it edits unprompted.
PROMPT="${1:-Read data.txt and tell me its second line. Create notes.md containing \
the single line 'checked'. In data.txt, change 'beta' to 'BETA'. Reply briefly.}"

# Run from inside another agent, its variables leak into Codex's hooks and
# read as Codex's own. The first capture was made from Claude Code and
# reported a CLAUDE_CODE_SESSION_ID that Codex never set.
UNSET=()
for v in $(compgen -e | grep -E '^CLAUDE'); do UNSET+=(-u "$v"); done

echo "== capturing into $OUT"
START=$(date +%s%N)
( cd "$PROJECT" && env "${UNSET[@]}" timeout 600 codex exec \
    --dangerously-bypass-hook-trust \
    --skip-git-repo-check \
    --sandbox workspace-write \
    "$PROMPT" < /dev/null ) > "$OUT/codex.log" 2>&1
# stdin from /dev/null: given an open stdin that is not a terminal, `codex
# exec` waits to read more prompt from it, and sat out the whole timeout.
STATUS=$?
END=$(date +%s%N)
echo "   codex exited $STATUS after $(( (END - START) / 1000000 )) ms (log: $OUT/codex.log)"
if [ "$STATUS" != 0 ]; then
    echo "   last lines of the log:"; tail -15 "$OUT/codex.log" | sed 's/^/     /'
    echo "   if a flag was rejected, \`codex exec --help\` shows this version's spelling"
fi

echo "== what fired"
ls "$CAPTURES" | sed -n 's/-[0-9]*-[0-9]*\.json$//p' | sort | uniq -c | sed 's/^/   /'

echo "== payload keys per event (values stay in the files)"
for f in "$CAPTURES"/*.json; do
    [ -e "$f" ] || continue
    python3 -I - "$f" <<'PY'
import json, sys
p = json.load(open(sys.argv[1]))
line = f"   {p.get('hook_event_name', '?'):<17} {sorted(p)}"
if "tool_name" in p:
    ti = p.get("tool_input")
    tr = p.get("tool_response")
    line += f"\n{'':21}tool_name={p['tool_name']!r} tool_input={type(ti).__name__}:{sorted(ti) if isinstance(ti, dict) else ''}"
    line += f" tool_response={type(tr).__name__} ({len(json.dumps(tr))} B)"
print(line)
PY
done

echo "== what a hook sees of the environment (names only; surfaces need TMUX/KITTY)"
cat "$CAPTURES"/*.env 2>/dev/null | sed 's/=.*//' | sort -u | tr '\n' ' ' | sed 's/^/   /'
echo

echo "== does an MCP server get a session id?"
if [ -s "$CAPTURES/mcp-server.env" ]; then
    grep -iE 'session|turn|codex|thread' "$CAPTURES/mcp-server.env" | sed 's/=.*/=…/; s/^/   /' \
        || echo "   no session-like variable in the MCP server's environment"
else
    echo "   the probe never started — check $OUT/codex.log"
fi

echo "== hook warnings Codex printed"
grep -iE 'hook|stop.*json|invalid' "$OUT/codex.log" | head -10 | sed 's/^/   /' || echo "   none"

echo
echo "Everything is in $OUT — read the captures, write docs/codex-hook-payload.md"
echo "from them, and delete the directory. Do not commit the raw files."
