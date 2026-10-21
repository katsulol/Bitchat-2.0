---
name: rust-build
description: Enforce build, format, lint, and test pass criteria for bitchat2_core.
---

# Rust Build & Verification Skill

## Commands
Execute from repo root:
1. `cargo fmt --check`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --all`

## Rule
All three commands must pass cleanly without warnings before any task is marked done.
No warnings allowed (`-D warnings`).
No `unwrap()` / `expect()` / `panic!` in non-test code.
