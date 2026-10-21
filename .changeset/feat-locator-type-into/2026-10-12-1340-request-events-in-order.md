# fix(events): handle a page's request events in the order they arrive

- **Date:** 2026-10-12 13:40
- **Commit:** 7d8a929
- **Area:** Events

## Problem

A page's request history could keep a request as unanswered forever although the browser had answered it. On a page issuing 1,200 requests at once, 17 of 1,202 never settled in one run and 4 in another; at 2,400 requests, 20 of 2,402. At 300 and 600 none were lost.

## Cause

`network.beforeRequestSent`, `network.responseCompleted` and `network.fetchError` each had a stream and a task of their own. The tasks ran independently, so when a request's two events arrived together the response could be handled first. It found no entry to settle and was dropped, and the entry the request event then added was never settled.

## Introduced

e8050d3 (2026-09-12), "feat(onday): WebDriver BiDi browser automation and an MCP server", which gave each event type its stream. abc5d61 (2026-10-01) put the navigation events on one ordered stream for the same reason and left these as they were.

## Fix

The three request events share one stream and are handled in the order they arrive.

## Verified

`cargo test -p onday --test flood -- --ignored`, with the fix and the mapper's log still in place: all 1,202 requests settled, 16.9 s into the flood. Then with both fixes, on Chromium and on Firefox: all 1,202 settled, 2.4 s and 3.2 s in.
