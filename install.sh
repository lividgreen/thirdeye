#!/bin/sh
# Install thirdeye from a release, then refresh its agent integrations.
#
#   curl -fsSL https://github.com/lividgreen/thirdeye/releases/latest/download/install.sh | sh
#
# POSIX sh, so it runs wherever `curl | sh` does. It verifies the archive's
# checksum, replaces the binary atomically, and hands over to the new
# binary's `thirdeye refresh`, which rewrites the files every installed
# integration loads. It edits no shell rc file.
#
# Knobs:
#   THIRDEYE_VERSION       a release, e.g. 0.2.0 (default: the latest)
#   THIRDEYE_INSTALL_DIR   where the binary goes (default: $XDG_BIN_HOME, else ~/.local/bin)
#   THIRDEYE_BASE_URL      where the archives are (default: the GitHub release)
#   --archive FILE         install this archive instead of downloading one;
#                          FILE.sha256 beside it is checked if present
set -eu

RELEASES="https://github.com/lividgreen/thirdeye/releases"

say() { printf 'thirdeye: %s\n' "$*"; }
die() { printf 'thirdeye: %s\n' "$*" >&2; exit 1; }

archive=""
while [ $# -gt 0 ]; do
    case "$1" in
        --archive) [ $# -ge 2 ] || die "--archive needs a file"; archive="$2"; shift 2 ;;
        --version) [ $# -ge 2 ] || die "--version needs a value"; THIRDEYE_VERSION="$2"; shift 2 ;;
        -h|--help)
            echo "usage: install.sh [--version V] [--archive FILE]"
            echo "env: THIRDEYE_VERSION, THIRDEYE_INSTALL_DIR, THIRDEYE_BASE_URL"
            exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

version="${THIRDEYE_VERSION:-}"
version="${version#v}"
install_dir="${THIRDEYE_INSTALL_DIR:-${XDG_BIN_HOME:-${HOME:?HOME is not set}/.local/bin}}"

# --- which build -----------------------------------------------------------
# musl, static: one archive runs on every distribution.
case "$(uname -s)" in
    Linux) os=unknown-linux-musl ;;
    Darwin) die "macOS has no release yet; build from source: cargo install thirdeye" ;;
    *) die "$(uname -s) has no release; on Windows use install.ps1 or winget" ;;
esac
case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) die "$(uname -m) has no release; build from source: cargo install thirdeye" ;;
esac
target="$arch-$os"
name="thirdeye-$target.tar.gz"

if [ -n "${THIRDEYE_BASE_URL:-}" ]; then
    base="${THIRDEYE_BASE_URL%/}"
elif [ -n "$version" ]; then
    base="$RELEASES/download/v$version"
else
    base="$RELEASES/latest/download"
fi

work="$(mktemp -d 2>/dev/null || mktemp -d -t thirdeye)"
trap 'rm -rf "$work"' EXIT INT TERM

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$1" -O "$2"
    else
        die "neither curl nor wget is installed"
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "neither sha256sum nor shasum is installed, so the download cannot be verified"
    fi
}

# --- the archive, verified --------------------------------------------------
if [ -n "$archive" ]; then
    [ -f "$archive" ] || die "$archive does not exist"
    cp "$archive" "$work/$name"
    if [ -f "$archive.sha256" ]; then
        cp "$archive.sha256" "$work/$name.sha256"
    else
        say "warning: no $archive.sha256 beside the archive; not verified"
    fi
else
    say "downloading $base/$name"
    fetch "$base/$name" "$work/$name" || die "could not download $base/$name"
    fetch "$base/$name.sha256" "$work/$name.sha256" || die "could not download its checksum"
fi
if [ -f "$work/$name.sha256" ]; then
    want="$(cut -d' ' -f1 < "$work/$name.sha256")"
    have="$(sha256 "$work/$name")"
    [ "$want" = "$have" ] || die "checksum mismatch for $name (expected $want, got $have); nothing installed"
fi

mkdir "$work/x"
tar -xzf "$work/$name" -C "$work/x" || die "$name is not a gzipped tar archive"
[ -f "$work/x/thirdeye" ] || die "$name holds no thirdeye binary"

# --- installed atomically ---------------------------------------------------
# Copied into the target directory first, then renamed over the old binary:
# a hook starting mid-install runs the old binary or the new one, never half
# of either. A running daemon keeps the old file open, which is fine.
mkdir -p "$install_dir"
binary="$install_dir/thirdeye"
first=1
[ -e "$binary" ] && first=0
cp "$work/x/thirdeye" "$install_dir/.thirdeye.new.$$"
chmod 755 "$install_dir/.thirdeye.new.$$"
mv -f "$install_dir/.thirdeye.new.$$" "$binary"
say "installed $("$binary" --version) at $binary"

# The new binary refreshes, not whatever ran this script: it is the one
# whose files the integrations must load.
"$binary" refresh --installed-by script >/dev/null || say "warning: \`thirdeye refresh\` failed; run it to see why"

case ":${PATH:-}:" in
    *":$install_dir:"*) ;;
    *) say "note: $install_dir is not on your PATH; add it, or run $binary" ;;
esac

if [ "$first" = 1 ]; then
    # Only with a terminal to ask on — `thirdeye install` asks about each
    # agent it finds, from the terminal rather than stdin, which under
    # `curl | sh` is this script. Not from the home directory, which is no
    # project to wire.
    # The repository's root, as the agents take it, from anywhere inside.
    here=$(git rev-parse --show-toplevel 2>/dev/null) || here=$(pwd -P)
    if [ "$here" != "$(cd "$HOME" && pwd -P)" ] && [ -t 1 ] && (exec </dev/tty) 2>/dev/null; then
        "$binary" install "$here" || say "later: \`thirdeye install <project>\`"
    else
        say "next: \`thirdeye install <project>\` finds your agents and asks which to wire"
    fi
fi
