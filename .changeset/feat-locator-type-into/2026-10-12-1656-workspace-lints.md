# chore(lints): hold the workspace to the Rust rules

- **Date:** 2026-10-12 16:56
- **Commit:** 3394985
- **Area:** Workspace

## Problem

Nothing checked the workspace against the rules its code is written to. It had no `[workspace.lints]`, no `clippy.toml` and no pinned toolchain, so `cargo clippy -- -D warnings` meant clippy's defaults on whatever toolchain was installed.

## Cause

The lint configuration was never written.

## Introduced

e8050d3 (2026-09-12), the first commit, which set up the workspace without one.

## Fix

`[workspace.lints]` denies what the rules ban outright and clippy has a lint for: `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, indexing and slicing outside tests; `as` casts that truncate, wrap, drop a sign or lose precision; unchecked arithmetic; a discarded `Result` or future; a guard held across an await; glob imports; single-letter names; `#[allow]`; and `unsafe`. Both crates inherit it. `rust-toolchain.toml` pins stable 1.99.0, the release the workspace was building with.

Left out on purpose, because the rules restrict these and do not ban them: `serde_json::Value` (allowed at an I/O boundary, which is most of this crate), `Result::ok` and its relatives, `tokio::spawn`, and a single bool parameter. `wildcard_enum_match_arm` is out too: it cannot tell an enum the crate owns from a borrowed one.

What the lints found, fixed in the same commit:

- 73 single-letter names. The coordinates are `horizontal` and `vertical`; serde renames keep `x` and `y` on the wire.
- 23 unchecked additions and subtractions, now saturating.
- The glob matcher, the frame table and the MCP server's tab choice no longer index or slice.
- Five broadcast sends dropped their `Result` with `.ok()`. `publish` sends and logs when the last subscriber left first.
- Seven glob imports are named.

Seven wildcard arms over the crate's own enums (`RemoteValue`, `Resolution`, `FrameTarget`, `WaitUntil`, `DriverSource`) are spelled out by hand.

## Verified

- `cargo clippy --workspace --all-targets --all-features -- -D warnings` exited 0.
- `cargo test --workspace` passed, 23 tests.
- `cargo test -p onday -- --ignored --test-threads=1`: the six browser scenarios (Chromium and Firefox, BiDi and Classic, closed tabs, a vanished browser) and both flood tests passed after the fixes, before the last five match arms were spelled out; the Chromium scenarios and the unit tests passed again after them.
