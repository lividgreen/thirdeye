# Contributing to thirdeye

Thanks for wanting to help. Bug reports, fixes, new agents and new terminals
are all welcome.

## Reporting a bug

Include the output of `thirdeye doctor <your project>`, the agent and terminal (with
versions), and what you expected to see. If the viewer showed nothing,
`$XDG_STATE_HOME/thirdeye/hook-errors.log` and `journalctl --user -u
'thirdeye@*'` usually say why.

Please do not paste your log files into an issue unredacted: they hold your
prompts and every path the agent touched.

## Building

You need:

- Rust 1.94 or newer (edition 2024), with `cargo`: <https://rustup.rs>
- git
- Linux, or Windows 10 or later with Git for Windows and the MSVC toolchain

```bash
git clone https://github.com/lividgreen/thirdeye
cd thirdeye
cargo build --release
```

The binary is `target/release/thirdeye` (`thirdeye.exe` on Windows). There
are no native dependencies beyond the Rust toolchain.

## Setting up

A build in `target/` is not what your agents run. To try a change against a
real agent, install it:

```bash
cargo xtask configure --agent claude=/path/to/a/test/project
cargo xtask install
```

`cargo xtask install` builds the binary, installs the daemon's units and
wires the agents you named, then checks that all of it agrees. Run it after
every change you want to try, and **restart your agent** afterwards: agents
read hooks and plugins at session start only. [`docs/development.md`](docs/development.md)
explains why the binary, the units and the integrations must always be
installed together.

## Before you open a pull request

All four must pass; lint failures are build failures here (`clippy::all` is
`deny`, `unsafe_code` is `forbid`):

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo xtask doctor
```

If you touched code behind `cfg(windows)` or `cfg(unix)`, lint the other
platform too — it is invisible to your own lints, and needs no linker:

```bash
rustup target add x86_64-pc-windows-msvc        # or x86_64-unknown-linux-gnu
cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
```

If you touched an adapter or a surface, also run the matching end-to-end
script in `scripts/` against the real agent or terminal, and say in the pull
request what you ran it against. Most of them use paid model credit; see
[`docs/e2e.md`](docs/e2e.md).

## How the code is written

[`CLAUDE.md`](CLAUDE.md) is the working guide: the architecture, the rules
that hold the seams together, and the gotchas, each learned the hard way. The
short version:

- **Producers and consumers never talk.** Hooks append to a log on disk; the
  daemon derives everything from it and owns no data of its own.
- **Adapters translate, heads consume.** No viewer reads an agent's format;
  adding an agent never touches a viewer, and the reverse.
- **Each seam is a directory, and each implementation a file.** `mod.rs`
  holds the seam and nothing else.
- **Facts are captured, not assumed.** Every payload shape and terminal
  command in this repo was observed against a live agent or terminal, and the
  `docs/*-payload.md` and per-terminal docs record what and against which
  version.
- Tests live at the bottom of each module and are named for the property
  they hold (`push_wins_over_pull_for_the_same_tool_call`, not `test_dedup`).
  A new indicator gets a test that is shown to fail on a planted defect.
- Comments say *why* a choice was made over the obvious alternative, not what
  the code does.

## Adding an agent

1. **Capture its hook payloads from a live session** into a throwaway
   project, and write them up as `docs/<agent>-hook-payload.md`: which events
   fire, their fields, which ids exist (call, turn, session), what the hook's
   environment and working directory are.
2. Write the adapter, `crates/thirdeye-adapters/src/<agent>.rs`, with tests
   built from the captured payloads. Structured tools are translated; anything
   that runs arbitrary commands is bracketed (`Observe`).
3. Add the installer, `crates/thirdeye/src/integration/<agent>.rs`
   implementing `Integration`, and a line in `registry()`: it merges into
   the agent's own configuration, removes exactly what it added, and says
   what it finds for `thirdeye doctor`. Then write `scripts/e2e-<agent>.sh`.

## Adding a terminal

1. Find out, against the real terminal, how to open a pane beside a given
   pane without taking focus, how to learn its id, and how to close it — or,
   if it cannot split, how to open a window from outside it.
2. Add `crates/thirdeye/src/daemon/surface/<terminal>.rs`, one line in
   `surface::ORDER`, and any variable it needs in `surface::SURFACE_ENV`.
3. Write `docs/<terminal>.md` saying what was verified and against which
   version.

## License

By contributing, you agree that your contributions are dual-licensed under
the MIT and Apache-2.0 licenses, as described in the [README](README.md#license).
