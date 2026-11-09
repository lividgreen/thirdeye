#!/usr/bin/env bash
# Feed a Claude Code hook payload to thirdeye — installed by `thirdeye install claude`.
#
# Exits 0 unconditionally: a broken viewer must never break the user's agent.
# Diagnostics go to $XDG_STATE_HOME/thirdeye/hook-errors.log, not to stderr,
# because stderr from a hook surfaces inside the agent session.

set -u
bin={{THIRDEYE}}
[ -x "$bin" ] || exit 0

errlog="${XDG_STATE_HOME:-${HOME:-}/.local/state}/thirdeye/hook-errors.log"
mkdir -p "$(dirname "$errlog")" 2>/dev/null || exit 0

"$bin" hook 2>>"$errlog" || true
exit 0
