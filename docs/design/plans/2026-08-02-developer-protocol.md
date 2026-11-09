# Plan — the developer protocol

For someone who clones the repo, changes code, and wants to see the change
running against a real agent. Distinct from `docs/install.md`, which is for
someone who just wants the tool.

The spec does not cover distribution, versioning or update at all — verified by
searching it. This is therefore a new decision, not the completion of a
recorded one.

## The problem, stated concretely

Three artifacts install by three unrelated mechanisms, and nothing links them:

| Artifact | Installed by | Noticed a change how? |
|---|---|---|
| binary | `cargo install --root ~/.local` | never — resolved from `PATH` at runtime |
| plugin | copied into `~/.claude/plugins/cache/…/<version>/` | only if the manifest version was bumped by hand |
| units | `thirdeye units install` | only if re-run |

So a contributor edits code, runs *some* of the three steps, restarts the
agent, and observes behaviour that belongs to a mixture of old and new. Every
component behaves correctly; the system is wrong.

**This is not hypothetical.** It happened during milestone 7: the plugin was
current, the binary was eighteen minutes stale, the hook recorded no kitty
address, the daemon correctly concluded "no surface", and nothing anywhere
reported a problem. That failure is the specification for this work.

## What a dev protocol has to give

1. **One command** that makes the installed set match the working tree.
2. **An answer to "what is actually running"** that does not require reading
   three directories by hand.
3. **The same code path as production**, so a dev install and a real install
   differ in profile and prefix, not in kind.

## Options

**A. A shell script (`./dev`).** Cheapest. Rejected: it would be the only part
of this repo that no test covers and no lint checks, in a project whose whole
discipline is that indicators are proven rather than trusted. Shell also makes
the "same code path as production" claim hard to keep honest.

**B. Subcommands on the `thirdeye` binary (`thirdeye dev install`).** No new
crate. Rejected for the build step on a chicken-and-egg ground: you would need
the newly built binary in order to install the newly built binary, and a stale
one would happily install itself again. It is the right home for *reporting*,
which is why `doctor` lands there.

**C. `cargo xtask` + `thirdeye doctor`.** ← **recommended**
A workspace member invoked as `cargo xtask dev`. It is ordinary Rust, so the
existing four gates cover it, it can call the binary's own unit installer
rather than reimplementing it, and it needs no tooling a contributor does not
already have. Reporting goes on the binary, where a non-contributor can also
reach it.

## The design

### D1 — `cargo xtask` orchestrates, `thirdeye doctor` reports

The split is not cosmetic. `xtask` acts *on* the environment and must be able
to run when the installed binary is broken or absent. `doctor` describes the
environment and is just as useful to someone who never cloned anything.

### D2 — a build stamp is what makes any of this checkable

The binary embeds `git describe --always --dirty` plus the plugin manifest
version at compile time. Without it, "is the installed binary this code?" has
no answer and every check below degrades to a guess.

`-dirty` is load-bearing for a contributor: it is the difference between
"running your edit" and "running your last commit".

### D3 — drift is detected by content, not by version numbers

`doctor` compares a hash of `plugin/` in the checkout against the installed
cache copy. Comparing declared versions would miss exactly the failure that
motivates this: files changed, version not bumped, `claude plugin update`
reporting "already at the latest version".

Conditional, and named as such: this check only runs when a checkout can be
located. Outside one, `doctor` reports what it can and says what it could not
check — a green that hid its own blind spot would be worse than no check.

### D4 — dev installs to the real locations

Tempting to sandbox everything into `target/`, and wrong. The thing under test
is an integration with a real agent: the plugin has to be somewhere Claude
Code loads plugins from, and the units have to be somewhere the user manager
reads. A sandbox would test a configuration nobody ships.

What *is* isolated is the **project** used for testing, not the paths.
`--prefix` remains for anyone who wants a different binary location.

Consequence, stated plainly: `cargo xtask dev` replaces the contributor's
working `thirdeye` with a debug build. `doctor` says so on every run, and
`cargo xtask install --release` puts a production build back.

### D5 — the plugin is reinstalled, not "updated"

`claude plugin update` compares manifest versions and no-ops when unchanged,
which is useless during development where the version legitimately does not
move between edits. `xtask` uninstalls and reinstalls, which forces the copy
and is deterministic regardless of version.

### D6 — production parity is one flag

`cargo xtask install --release` runs the identical steps at the release
profile. Dev and production differ in profile and prefix; nothing else forks.
This is what keeps "deterministic way to build production artifacts that match
dev" true rather than aspirational.

## Scope

In: `xtask` with `dev`, `install`, `doctor`, `clean`; `thirdeye doctor`; the
build stamp; `docs/development.md`.

Out: releases, tags, CI, prebuilt binaries, crates.io. The human has scheduled
distribution for after the PoC.

## What the checks will not cover

- **Whether the running agent session predates the install.** Nothing on disk
  records when a session read its plugin config, so `doctor` can only say that
  a restart is required after any install, never that one is outstanding.
- **A plugin installed from somewhere other than this checkout.** The content
  hash then reports drift that is not drift; it names the two paths so the
  reader can see why.
- **macOS.** The unit half is systemd-only and reports as unavailable rather
  than failing.


---

# Revision — configure, then install

The first cut was rejected by the human on three counts, all correct.

## R1 — probing and acting were the same step

`cargo xtask dev` decided where things go *while* putting them there, re-deriving
its answers on every run. Nothing recorded what the environment looked like, so
nothing could be reviewed before it acted, and a machine where a guess was wrong
had no place to correct it.

**Adopt the configure/install split.** `configure` probes and writes
`install.toml`; `install` reads that file and does exactly what it says, with no
probing and no defaults of its own. The file is gitignored — it describes one
machine, not the project. Re-running `configure` is how you change your mind;
`install` never quietly disagrees with it.

This also gives `doctor` something to check *against*: not "does the tree look
plausible" but "does what is installed match what was configured".

## R2 — the test directory was baked in

The first cut defaulted the plugin target to a sibling `../thirdeye-test`. That
is the author's own directory layout compiled into the repo. It is invisible to
anyone else, and it silently installs into a path they never named.

**No default target.** `configure` records the projects the developer names and
no others. With none named, `install` puts down the binary and the units, says
that no agent was wired, and prints the command that would do it. Installing
into someone's project is a side effect and must be asked for.

## R3 — the installer assumed one agent

The worst of the three, because the project already forbids it. §5 makes Adapter
one of the five seams and CLAUDE.md states that adding an agent must not touch
head code — and then the installer hardcoded `claude plugin install`. A user
with codex or opencode, alone or alongside Claude, could not be served, and the
installer contradicted the architecture it installs.

**An agent registry behind a trait.** `Agent` has `id`, `detect`, `install`,
`uninstall`. Claude Code is one implementation. Others are *detected and named*
as unsupported rather than ignored or faked — thirdeye has exactly one adapter
today (`thirdeye-adapters::claude`), and an installer that implied otherwise
would be lying about the product.

Status of that claim, stated plainly: **established** that the seam exists in
the core; **open** what a codex or opencode integration would look like, since
neither is installed here and their plugin mechanisms have not been verified.
`configure` therefore reports detection only, and never invents a wiring.

## The commands after the revision

| Command | Does |
|---|---|
| `cargo xtask configure [flags]` | probe, print findings, write `install.toml` |
| `cargo xtask install` | read `install.toml`, build, install binary + units + agents |
| `cargo xtask uninstall` | reverse it, using the same file |
| `cargo xtask doctor` | compare installed state against `install.toml` and the source |

`dev` and `clean` are gone: they named a workflow rather than a step, and their
defaults were the two mistakes above.
