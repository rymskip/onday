// Expects `el` in scope. Scrolls only the nearest user-scrollable ancestor (or
// the window), the way dragging that container's scrollbar would.
function findScrollable(node) {
    for (let p = node.parentElement; p; p = p.parentElement) {
        const cs = getComputedStyle(p);
        const overflowY = cs.overflowY;
        const canScroll = (overflowY === 'auto' || overflowY === 'scroll') && p.scrollHeight > p.clientHeight;
        if (canScroll) return p;
    }
    return document.scrollingElement || document.documentElement;
}
const __container = findScrollable(el);
const __isWin = __container === document.scrollingElement || __container === document.documentElement;
const __cRect = __isWin
    ? { top: 0, bottom: window.innerHeight, height: window.innerHeight }
    : __container.getBoundingClientRect();
const __eRect = el.getBoundingClientRect();
let __delta = 0;
if (__eRect.top < __cRect.top) __delta = __eRect.top - __cRect.top;
else if (__eRect.bottom > __cRect.bottom) __delta = __eRect.bottom - __cRect.bottom;
if (__delta !== 0) {
    if (__isWin) window.scrollBy(0, __delta);
    else __container.scrollTop += __delta;
}
// A sticky header or footer can cover a target scrolled flush to the edge:
// probe the click point and nudge by the measured overlap, a few passes at most.
for (let __pass = 0; __pass < 3; __pass++) {
    const __r = el.getBoundingClientRect();
    if (__r.width === 0 || __r.height === 0) break;
    const __hit = document.elementFromPoint(
        Math.floor(__r.left + __r.width / 2),
        Math.floor(__r.top + __r.height / 2)
    );
    // An ancestor coming back means the target is not hit-testable; scrolling cannot fix that.
    if (!__hit || __hit === el || el.contains(__hit) || __hit.contains(el)) break;
    // Clear the whole sticky or fixed bar, not just the piece of it that was hit.
    let __cover = __hit;
    for (let __p = __hit; __p && __p !== document.body; __p = __p.parentElement) {
        const __pos = getComputedStyle(__p).position;
        if (__pos === 'sticky' || __pos === 'fixed') __cover = __p;
    }
    const __hr = __cover.getBoundingClientRect();
    let __fix = 0;
    if (__hr.top <= __r.top && __hr.bottom > __r.top) __fix = __hr.bottom - __r.top;
    else if (__hr.bottom >= __r.bottom && __hr.top < __r.bottom) __fix = __hr.top - __r.bottom;
    if (__fix === 0) break;
    if (__isWin) window.scrollBy(0, -__fix);
    else __container.scrollTop -= __fix;
}
