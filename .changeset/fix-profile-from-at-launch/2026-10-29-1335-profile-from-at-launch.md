# fix(mcp): copy another session's profile at browser launch, not at server start

- **Date:** 2026-10-29 13:35
- **Commit:** 0b165ae
- **Area:** MCP server

## Problem

Starting a browser from another session's profile only worked as `--profile-from`, read when the server starts. Carrying a browser's extensions, logins and storage into a running session meant editing the MCP config and restarting the server, which is what the option was meant to avoid.

## Cause

The source session was settled in `Session::new` from the command line, and the copy ran only when the session had no profile yet, so a session already in use could never take one.

## Introduced

bbef0c5 (2026-10-28) `fix(page): restore a frame whose document chromedriver lost when it moved process`, released as 0.4.1, which also added `--profile-from` as a startup option.

## Fix

`browser_launch` takes `profile_from`. It checks the session id first, then closes the running browser, replaces this session's profile for the engine with a copy of the other session's, and relaunches on it. The copy is built under `.partial`, the old profile is moved aside to `.replaced` and removed once the copy is in place, and leftovers of an interrupted replacement are cleared first. Copying this session's own profile, a session open in another process, or into an isolated session is refused. A later relaunch keeps the copied profile. `--profile-from` and `ONDAY_PROFILE_FROM` are removed.

## Verified

- `cargo test -p onday_mcp`: 10 tests pass, including an existing profile replaced and a stale `.replaced` cleared, and a source resolved beside this session.
- `cargo test -p onday_mcp a_copied_chromium_profile -- --ignored` passes.
- The built `onday_mcp` over stdio: session `earlier` stored a cookie and local storage and exited; `later` started empty, was refused its own profile and `earlier` while another process held it, then `browser_launch` with `profile_from: earlier` read both values back, and a plain relaunch kept them.
