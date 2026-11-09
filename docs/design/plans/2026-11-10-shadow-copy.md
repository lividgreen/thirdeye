# Run the daemon and MCP server from a per-build copy (Windows)

## Problem

A Claude session holds the daemon and the MCP server — both `thirdeye.exe` —
for its whole life. winget deletes rather than renames; with either running,
`winget upgrade` fails with "remove: Access is denied", half-deletes the
package and still records the new version (live, 2026-11-09, Windows 11).

## Design

`shadow.rs`: `<data>/bin/<build>/thirdeye.exe`, a copy of the binary, made
atomically (copy to a `.part` name, rename). `run_path(data, binary)` returns
it on Windows and `binary` itself elsewhere, where a running exe can be
replaced — and also when the copy cannot be made, so the failure costs only
the upgrade.

- **Daemon**: `notify::start_daemon` starts the copy, not `current_exe()`.
  The daemon's surfaces already launch the head from the daemon's own path.
- **MCP server (Claude)**: publish writes the copy's path as `.mcp.json`'s
  `command`. Hooks keep naming the package manager's path: they live ~50 ms
  and are what notices a new build. `published_binary` can no longer read the
  hook binary from `command`, so a server running a copy records it in
  `env.THIRDEYE_PUBLISHED_BINARY`.
- **Prune**: each `run_path` deletes other builds' directories. One a session
  still runs cannot be deleted and goes at the next prune.

Deviation from the memo: the copies live under the data directory
(`%LOCALAPPDATA%\thirdeye-data`), not the state directory, which people are
told they may delete to clear logs.

## Not done

- Other agents' MCP configs on Windows (Codex, Copilot, …) still name the
  installed binary; only Claude is verified on Windows.
- Live check: `winget upgrade` with a daemon and `thirdeye mcp` running must
  succeed.
