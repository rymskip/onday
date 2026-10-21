# feat/locator-type-into changes

In commit order.

| Date | Commit | Change | Introduced |
| --- | --- | --- | --- |
| 2026-10-12 13:40 | 7d8a929 | [fix(events): handle a page's request events in the order they arrive](2026-10-12-1340-request-events-in-order.md) | e8050d3 (2026-09-12) |
| 2026-10-12 15:04 | 02c9830 | [fix(launch): remove the log chromedriver's BiDi tab renders every message into](2026-10-12-1504-mapper-log.md) | e8050d3 (2026-09-12) |
| 2026-10-12 15:54 | 5eed46d | [feat(locator): type into a field like a person and check what it holds](2026-10-12-1554-locator-type-into.md) | — |
| 2026-10-12 15:57 | 92988e1 | [feat(locator): let a locator carry its own timeout, and wait on it](2026-10-12-1557-locator-timeout.md) | e8050d3 (2026-09-12) |
| 2026-10-12 16:56 | 3394985 | [chore(lints): hold the workspace to the Rust rules](2026-10-12-1656-workspace-lints.md) | e8050d3 (2026-09-12) |
| 2026-10-12 18:01 | 9cc5f83 | [feat(page): stroke with the mouse held down, and an element's box to aim it](2026-10-12-1801-mouse-stroke.md) | — |
| 2026-10-12 18:22 | b2df5c2 | [feat(page): reach a tab's context, each match of a locator, and the raw driver on that tab](2026-10-12-1822-page-context-and-matches.md) | — |
| 2026-10-12 19:05 | d1a253a | [feat(page): turn the wheel with the pointer at a point](2026-10-12-1905-wheel-at.md) | — |
| 2026-10-14 17:22 | 56348d7 | [fix(page): run the runtime in a sandbox the page cannot reach](2026-10-14-1722-runtime-sandbox.md) | 1f060d1 (2026-09-17) |
| 2026-10-14 22:56 | 0ee6342 | [fix(page): move a frame to a fresh sandbox when the driver loses onday's](2026-10-14-2256-fresh-sandbox.md) | 56348d7 (2026-10-14) |
| 2026-10-15 12:12 | 4c16a73 | [fix(locator): check the hit when typing into a field, and re-scroll as Playwright does when it misses](2026-10-15-1212-clipped-aim.md) | 5eed46d (2026-10-12) |
| 2026-10-15 12:33 | 12e8993 | [fix(page): handle a dialog's opening and closing in order, and drop it from the record when answered](2026-10-15-1233-stale-dialog.md) | e8050d3 (2026-09-12) |
| 2026-10-15 17:58 | eb9fff7 | [fix(page): capture an element over BiDi as a box of the document, as Playwright does](2026-10-15-1758-element-shot-box.md) | e8050d3 (2026-09-12) |
| 2026-10-15 20:50 | 405a31f | [fix(events): record when a request settled by onday's own clock](2026-10-15-2050-settled-at.md) | e8050d3 (2026-09-12) |
| 2026-10-18 11:58 | b8d65e0 | [fix(locator): scroll a target taller than its box once, as Playwright does](2026-10-18-1158-tall-target-scroll.md) | e8050d3 (2026-09-12) |
| 2026-10-18 11:58 | ef2d74a | [fix(locator): drop only where the target takes the pointer, as Playwright does](2026-10-18-1158-drop-hit-target.md) | e8050d3 (2026-09-12) |
| 2026-10-18 12:17 | 1e23ff6 | [fix(locator): finish a drag whose page re-renders its source on drop](2026-10-18-1217-drop-rerenders-source.md) | e8050d3 (2026-09-12) |
| 2026-10-18 14:16 | 60b3471 | [feat(page): evaluate in onday's own world with inspect](2026-10-18-1416-inspect.md) | none (new API) |
| 2026-10-18 17:04 | 09929cc | [fix(locator): drag in Playwright's steps, one move onto the target](2026-10-18-1704-drag-steps.md) | e8050d3 (2026-09-12) |
| 2026-10-18 23:37 | 6529dd2 | [fix(locator): scroll before an action as Playwright does over BiDi](2026-10-18-2337-scroll-as-playwright.md) | 1f060d1 (2026-09-17) |
| 2026-10-19 14:29 | e905d65 | [fix(locator): press the drag source before aiming the target, as Playwright does](2026-10-19-1429-drag-press-before-aim.md) | e8050d3 (2026-09-12) |
| 2026-10-19 15:04 | 6e47771 | [fix(locator): check an action's target holds still before scrolling it, as Playwright does](2026-10-19-1504-stable-before-scroll.md) | 6529dd2 (2026-10-18) |
| 2026-10-19 15:37 | 1886433 | [feat(locator): take pointer actions through Playwright's retry and hit-target guard](2026-10-19-1537-pointer-actions.md) | e8050d3 (2026-09-12) |
| 2026-10-19 15:53 | 2c5ce6b | [fix(locator): name every refused attempt, and what refused a fallback drop](2026-10-19-1553-name-refusals.md) | 1886433 (2026-10-19) |
| 2026-10-19 21:28 | 6d2e5e6 | [feat(events): keep a console message's whole stack](2026-10-19-2128-console-stack.md) | e8050d3 (2026-09-12) |
| 2026-10-20 11:08 | c11ad3f | [fix(locator): retry a pointer action whose target's frame restarts](2026-10-20-1108-retry-restarting-frame.md) | 1886433 (2026-10-19) |
| 2026-10-20 12:10 | 7d21aff | [fix(locator): scroll a visible target only when needed on the first attempt](2026-10-20-1210-scroll-if-needed.md) | 1886433 (2026-10-19) |
| 2026-10-21 13:37 | 6cf2156 | [fix(locator): leave a visible target unscrolled on Firefox too](2026-10-21-1337-firefox-scroll-if-needed.md) | 7d21aff (2026-10-20), 6d2e5e6 (2026-10-19) |
