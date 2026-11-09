#!/usr/bin/env bash
# Exercise install.sh and `thirdeye self update | uninstall` against a
# locally built archive, served over HTTP, into a scratch HOME — no GitHub,
# no release, nothing of the developer's touched. Free; needs python3.
#
#   scripts/test-install.sh            # builds the dev binary first
#   THIRDEYE_TEST_BINARY=… scripts/test-install.sh
#
# The archive is named for the musl target install.sh picks, whatever the
# binary inside was built for: what is tested is the script, not the build.
set -u

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRATCH="$(mktemp -d -t thirdeye-install-XXXXXX)"
PASS=0
FAIL=0
pass() { printf '  \033[32mok\033[0m    %s\n' "$*"; PASS=$((PASS + 1)); }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; FAIL=$((FAIL + 1)); }
step() { printf '\n==> %s\n' "$*"; }

SERVER=""
cleanup() {
    [ -n "$SERVER" ] && kill "$SERVER" 2>/dev/null
    rm -rf "$SCRATCH"
}
trap cleanup EXIT

if [ -z "${THIRDEYE_TEST_BINARY:-}" ]; then
    ( cd "$REPO" && cargo build -q -p thirdeye ) || { fail "build"; exit 1; }
    THIRDEYE_TEST_BINARY="$REPO/target/debug/thirdeye"
fi

case "$(uname -m)" in aarch64|arm64) arch=aarch64 ;; *) arch=x86_64 ;; esac
NAME="thirdeye-$arch-unknown-linux-musl.tar.gz"

step "a release, served locally"
mkdir -p "$SCRATCH/release/stage" "$SCRATCH/home"
cp "$THIRDEYE_TEST_BINARY" "$SCRATCH/release/stage/thirdeye"
tar -czf "$SCRATCH/release/$NAME" -C "$SCRATCH/release/stage" thirdeye
( cd "$SCRATCH/release" && sha256sum "$NAME" > "$NAME.sha256" )
cp "$REPO/install.sh" "$SCRATCH/release/install.sh"
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$SCRATCH/release" >/dev/null 2>&1 &
SERVER=$!
for _ in $(seq 50); do
    curl -fs "http://127.0.0.1:$PORT/install.sh" >/dev/null 2>&1 && break
    sleep 0.1
done
pass "serving $NAME on port $PORT"

# Everything thirdeye or an agent might write goes under the scratch HOME.
export HOME="$SCRATCH/home"
export XDG_DATA_HOME="$HOME/.local/share" XDG_STATE_HOME="$HOME/.local/state"
export XDG_CONFIG_HOME="$HOME/.config" XDG_BIN_HOME="$HOME/.local/bin"
unset THIRDEYE_DATA_DIR THIRDEYE_STATE_DIR THIRDEYE_INSTALL_DIR THIRDEYE_VERSION CLAUDE_PROJECT_DIR
export THIRDEYE_BASE_URL="http://127.0.0.1:$PORT"
BIN="$XDG_BIN_HOME/thirdeye"

step "curl | sh, as a user runs it"
out=$(curl -fsSL "$THIRDEYE_BASE_URL/install.sh" | sh 2>&1)
[ -x "$BIN" ] && pass "binary installed at $BIN" || fail "no binary: $out"
grep -q 'source = "script"' "$XDG_DATA_HOME/thirdeye/published.toml" 2>/dev/null \
    && pass "published.toml says the script installed it" \
    || fail "published.toml: $(cat "$XDG_DATA_HOME/thirdeye/published.toml" 2>&1)"
grep -q "^build = \"$("$BIN" --version | cut -d' ' -f2-)\"" "$XDG_DATA_HOME/thirdeye/published.toml" 2>/dev/null \
    && pass "the new binary refreshed" || fail "not refreshed by this build"
echo "$out" | grep -q "not on your PATH" && pass "warned that the directory is not on PATH" \
    || fail "no PATH warning: $out"
echo "$out" | grep -q "thirdeye install <project>" && pass "pointed at install, asking nothing without a terminal" \
    || fail "no install hint: $out"
[ -e "$HOME/.bashrc" ] || [ -e "$HOME/.profile" ] \
    && fail "a shell rc file was written" || pass "no shell rc file touched"

step "a checksum that does not match"
before=$(sha256sum "$BIN" | cut -d' ' -f1)
cp "$SCRATCH/release/$NAME.sha256" "$SCRATCH/good.sha256"
echo "0000000000000000000000000000000000000000000000000000000000000000  $NAME" \
    > "$SCRATCH/release/$NAME.sha256"
if curl -fsSL "$THIRDEYE_BASE_URL/install.sh" | sh >"$SCRATCH/bad.log" 2>&1; then
    fail "installed despite a bad checksum"
else
    grep -q "checksum mismatch" "$SCRATCH/bad.log" && pass "refused: $(tail -1 "$SCRATCH/bad.log")" \
        || fail "failed, but not for the checksum: $(cat "$SCRATCH/bad.log")"
fi
[ "$(sha256sum "$BIN" | cut -d' ' -f1)" = "$before" ] && pass "the installed binary is untouched" \
    || fail "the binary changed"
cp "$SCRATCH/good.sha256" "$SCRATCH/release/$NAME.sha256"

step "thirdeye self update"
mkdir -p "$SCRATCH/project" && ( cd "$SCRATCH/project" && git init -q )
"$BIN" install -a codex "$SCRATCH/project" --no-units >/dev/null 2>&1 \
    && pass "wired codex into a project" || fail "install codex"
project_before=$(find "$SCRATCH/project/.codex" -type f -o -type l | sort | xargs -r sha256sum 2>/dev/null)
sed -i 's/^build = .*/build = "0.0.1 (old)"/' "$XDG_DATA_HOME/thirdeye/published.toml"
if "$BIN" self update >"$SCRATCH/update.log" 2>&1; then
    pass "self update ran the installer"
else
    fail "self update: $(cat "$SCRATCH/update.log")"
fi
grep -q "^build = \"$("$BIN" --version | cut -d' ' -f2-)\"" "$XDG_DATA_HOME/thirdeye/published.toml" \
    && pass "and the new binary republished thirdeye's directory" \
    || fail "published.toml: $(cat "$XDG_DATA_HOME/thirdeye/published.toml")"
project_after=$(find "$SCRATCH/project/.codex" -type f -o -type l | sort | xargs -r sha256sum 2>/dev/null)
[ "$project_before" = "$project_after" ] && pass "the project is byte for byte as it was" \
    || fail "the update rewrote the project"

step "an update whose download fails says so"
if THIRDEYE_BASE_URL="http://127.0.0.1:1" "$BIN" self update >"$SCRATCH/down.log" 2>&1; then
    fail "self update reported success with nothing to download"
else
    pass "self update failed: $(tail -1 "$SCRATCH/down.log")"
fi
[ -x "$BIN" ] && pass "and the binary is still there" || fail "the binary is gone"

step "--archive, without a server"
kill "$SERVER" 2>/dev/null; SERVER=""
THIRDEYE_BASE_URL="http://127.0.0.1:1" sh "$SCRATCH/release/install.sh" \
    --archive "$SCRATCH/release/$NAME" >"$SCRATCH/archive.log" 2>&1 \
    && pass "installed from a local archive" || fail "--archive: $(cat "$SCRATCH/archive.log")"

step "a package manager's binary is not ours to replace"
mkdir -p "$HOME/.cargo/bin" && cp "$BIN" "$HOME/.cargo/bin/thirdeye"
if "$HOME/.cargo/bin/thirdeye" self update >"$SCRATCH/cargo.log" 2>&1; then
    fail "self update replaced a cargo-installed binary"
else
    grep -q "cargo install thirdeye --force" "$SCRATCH/cargo.log" \
        && pass "refused, naming cargo install" || fail "$(cat "$SCRATCH/cargo.log")"
fi

step "a project is checked and unwired by naming it"
"$BIN" doctor "$SCRATCH/project" 2>&1 | grep -q "^  ok        codex: " \
    && pass "doctor checks the project it is given" \
    || fail "doctor: $("$BIN" doctor "$SCRATCH/project" 2>&1)"
"$BIN" doctor >/dev/null 2>&1 && fail "doctor ran with neither a project nor --user" \
    || pass "doctor wants a project or --user"
"$BIN" uninstall "$SCRATCH/project" >"$SCRATCH/unwire.log" 2>&1 \
    && grep -q "codex: removed" "$SCRATCH/unwire.log" \
    && pass "uninstall with no agent named removed what was wired there" \
    || fail "uninstall: $(cat "$SCRATCH/unwire.log")"
[ ! -e "$SCRATCH/project/.codex" ] && pass "the project's wiring removed" \
    || fail "left in the project: $(find "$SCRATCH/project/.codex")"

step "thirdeye self uninstall"
"$BIN" self uninstall >"$SCRATCH/uninstall.log" 2>&1 \
    && pass "self uninstall ran" || fail "$(cat "$SCRATCH/uninstall.log")"
grep -q "cannot unwire them" "$SCRATCH/uninstall.log" \
    && pass "it warned that projects are not unwired" || fail "no warning about projects"
[ ! -e "$BIN" ] && pass "binary removed" || fail "binary still there"
[ ! -e "$XDG_DATA_HOME/thirdeye" ] && pass "thirdeye's directory removed" \
    || fail "data left: $(find "$XDG_DATA_HOME/thirdeye")"

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" = 0 ]
