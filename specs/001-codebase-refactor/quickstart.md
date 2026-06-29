# Quickstart: Validate TUI Module Refactor

**Feature**: [spec.md](./spec.md)
**Date**: 2026-06-27

This guide validates that the refactor is correct after each step. Run these
checks after every commit — not just at the end.

## Prerequisites

- Rust stable toolchain (`rustup show`)
- Linux system dependencies (see `CLAUDE.md`)
- A running Docker registry (local or remote) for manual smoke test

---

## After Every Step: Automated Gates

```sh
# 1. Compile check (catches moves that broke imports)
cargo build

# 2. Lint (must pass with zero warnings — constitution gate)
cargo clippy -- -D warnings

# 3. Test suite (all existing 25 tests + new App tests must pass)
cargo test

# 4. Format check
cargo fmt --check
```

All four must pass before committing a step.

---

## After Step 1: Verify App method extraction

Confirm the new methods exist and are reachable:

```sh
# Grep should find the new methods in app.rs
grep -n "pub fn start_copy_image\|pub fn start_retag\|pub fn start_delete\|pub fn start_export\|pub fn start_diff\|pub fn start_registry_select\|pub fn copy_pull_url" src/tui/app.rs
```

Expected: 7 matches, all in `src/tui/app.rs`.

---

## After Step 2: Verify new tests exist and pass

```sh
cargo test tui
```

Expected: at least 12 new tests pass, no terminal or network required. All run
in < 1 second.

```sh
# Count tests in app.rs
grep -c "#\[test\]" src/tui/app.rs
```

Expected: ≥ 12.

---

## After Step 3: Verify file sizes

```sh
# tui/mod.rs must be ≤ 80 lines
wc -l src/tui/mod.rs

# event.rs must have grown substantially
wc -l src/tui/event.rs

# Verify no tokio::spawn in mod.rs or app.rs
grep -n "tokio::spawn" src/tui/mod.rs src/tui/app.rs
```

Expected:
- `src/tui/mod.rs`: ≤ 80 lines
- `src/tui/event.rs`: ~700 lines
- No `tokio::spawn` matches in `mod.rs` or `app.rs`

---

## After Step 4: Verify suppressors removed

```sh
# No allow(dead_code) in app.rs or event.rs
grep "allow(dead_code)" src/tui/app.rs src/tui/event.rs
```

Expected: no output.

```sh
# Final lint pass — must be clean
cargo clippy -- -D warnings
```

---

## Manual Smoke Test (after all steps)

Run against a real or local registry to confirm no functional regression:

```sh
cargo build --release
./target/release/docker-registry-walk
```

Exercise these flows manually:
1. Navigate repos with `j`/`k` — repo list scrolls, tags load automatically
2. Switch to tags panel with Tab — tag list responds to keys
3. Press `/` to filter repos — typing filters the list
4. Press `c` — pull URL copied to clipboard and status bar shows confirmation
5. Press `C` — copy-image modal opens with prefilled `repo:tag`
6. Press `r` — retag modal opens
7. Press `d` — delete confirmation appears
8. Press `i` — inspect modal opens with JSON
9. Press `e` — export modal opens
10. Press `D` — diff modal opens
11. Press `?` — help modal opens with keybindings
12. Press `R` — registry select modal opens (if multiple registries configured)
13. Press `q` — application exits cleanly, terminal restored

Expected: all flows work identically to before the refactor.
