# WezTerm Surface Implementation Plan

**Goal:** the viewer opens in a WezTerm split beside the agent, as it does in
tmux and kitty.

**Architecture:** one file, `daemon/surface/wezterm.rs`, and one registry line
— the seam working as designed. Facts verified live against wezterm
20260929-043349-cab25161 on 2026-10-08, before this plan:

- The daemon reaches the right GUI instance through the socket in
  `$WEZTERM_UNIX_SOCKET` (`/run/user/<uid>/wezterm/gui-sock-<pid>`), passed in
  the environment of the `wezterm cli` call. The hook records it with
  `$WEZTERM_PANE`, the agent's own pane.
- **Every call passes `--no-auto-start`.** Without it, `wezterm cli` with no
  reachable GUI spawns a `wezterm-mux-server --daemonize` — found by doing
  exactly that during research.
- `split-pane --pane-id <agent> --right --percent 40 --cwd <ws> -- <argv>`
  prints the new pane's id. The command is argv, not a shell string: no
  quoting to get wrong.
- **`split-pane` steals focus** within the tab: the new pane becomes the tab's
  active pane. `activate-pane --pane-id <previous>` restores it and leaves the
  new pane visible. The client stays on whatever tab the human is viewing.
  The pane to restore is the one that was active *in the agent's tab* before
  the split, read from `list --format json` (`tab_id`, `is_active`) — not the
  agent's pane, which the human may not have been in.
- `kill-pane --pane-id <id>` closes exactly that pane; a gone pane exits 1.

**Constraints:** the usual gates; no new crates; tests assert exact argv with
a recording runner that returns scripted stdout per call.

## Task 1: the surface

- [ ] `SURFACE_ENV` gains `WEZTERM_UNIX_SOCKET`, `WEZTERM_PANE`; `ORDER`
  becomes tmux, wezterm, kitty (a multiplexer inside either emulator still
  wins); `build` gains the arm; doctor's no-surface wording names WezTerm.
- [ ] Tests: `the_instance_is_named_by_its_socket_and_never_started`;
  `the_split_lands_beside_the_agents_own_pane`;
  `focus_returns_to_the_pane_that_had_it`;
  `the_head_runs_as_argv_so_a_session_id_cannot_reach_a_shell`;
  `closing_targets_the_pane_id_wezterm_gave_us`; `opening_twice_splits_once`;
  `a_split_that_prints_no_pane_id_leaves_the_surface_closed`;
  `a_pane_the_human_already_closed_does_not_trap_the_surface`;
  `an_agent_outside_wezterm_has_no_wezterm_surface`;
  registry: `a_multiplexer_beats_the_emulator_hosting_it` covers wezterm.
- [ ] Perturb: restore the agent's pane instead of the tab's active one; the
  focus test goes red.

## Task 2: live verification and docs

- [ ] Drive `thirdeye hook` from inside a WezTerm pane (a private instance)
  with captured payload shapes, as `docs/tmux.md` was verified: the split
  opens unfocused, hides and reopens, closes at `SessionEnd`.
- [ ] `docs/wezterm.md`, CLAUDE.md (seam table, Built paragraph).
