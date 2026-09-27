/**
 * jsdom gaps the browser script depends on, filled in before any test imports it.
 *
 * `scrollIntoView` is simply not implemented by jsdom, and the turn index calls
 * it every time it highlights an anchor. Patching here rather than stubbing the
 * call site keeps the script under test free of test-only branches.
 */
if (typeof Element.prototype.scrollIntoView !== 'function') {
  Element.prototype.scrollIntoView = function scrollIntoView(): void {
    // No layout in jsdom, so there is nothing to scroll.
  };
}
