---
title: thirdeye
summary: "Observes your coding agent and highlights the files it touches: a live list of what it reads and changes, each opened at the lines it worked on."
tags: [coding-agents, claude-code, codex, tui, rust]
status: active
---

## What it is

thirdeye runs beside a command-line coding agent and shows which files the agent is working on, while it works.
It keeps a live list of the files the agent has read, edited, written, deleted or searched, grouped by prompt and ranked by how much the agent did with each.
Selecting a file opens it at the lines the agent touched, or shows its diff.
The agent can also flag a file it wants a person to look at: a flagged file goes to the top of the list, with the agent's reason.

The list opens by itself: as a pane beside the agent in tmux, Zellij, WezTerm or kitty, and as a separate window in terminals that cannot split.

## Where it runs

thirdeye works with Claude Code, Codex CLI, GitHub Copilot CLI and other command-line coding agents, on Linux, in WSL and on Windows, where it supports Claude Code only for now.
The [README](../README.md#what-is-supported) lists each agent, terminal and platform with the version it was verified against, and [Install](install.md) has the commands.

## How it works

Each agent's hooks, or its plugin, hand every tool call to thirdeye, which appends what the call did to a log on disk, one per session.
A daemon per session derives the list from that log and opens the viewer in the agent's terminal; it can be stopped or restarted without losing anything.
Everything stays on the machine: the log holds paths and prompts, never file contents, and nothing is sent over the network.

## Status

thirdeye is in early development.
Every agent and terminal it supports has been verified end to end against a live session, but its command line, its log format and its viewer may still change before v0.1.0.
Prebuilt binaries for Linux and Windows are on the [releases page](https://github.com/lividgreen/thirdeye/releases), and the [changelog](../CHANGELOG.md) lists what changed.

thirdeye is written in Rust and licensed under MIT or Apache-2.0.
