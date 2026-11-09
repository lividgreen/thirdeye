## What and why

<!-- What changes, and why this way rather than the obvious alternative. -->

## Checked

- [ ] `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`
- [ ] For `cfg(windows)` or `cfg(unix)` code: clippy for the other target as well
- [ ] For an adapter or a surface: the matching `scripts/e2e-*.sh`, run against <!-- agent or terminal, and version -->
- [ ] `CHANGELOG.md`, under Unreleased, if a user would notice
