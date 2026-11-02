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

/**
 * jsdom ships <dialog> without the top layer: showModal() and close() are
 * missing, and the delete confirmation cannot be driven without them. Mirrored
 * here are only the two things the script observes — the open attribute and the
 * close event — so a test still sees whether the dialog was dismissed.
 * showModal returns a promise because Chrome's does; nothing awaits it.
 */
if (typeof HTMLDialogElement.prototype.showModal !== 'function') {
  HTMLDialogElement.prototype.showModal = function showModal(this: HTMLDialogElement): Promise<void> {
    this.setAttribute('open', '');
    return Promise.resolve();
  };
}

if (typeof HTMLDialogElement.prototype.close !== 'function') {
  HTMLDialogElement.prototype.close = function close(
    this: HTMLDialogElement,
    returnValue?: string,
  ): void {
    if (returnValue !== undefined) {
      this.returnValue = returnValue;
    }
    this.removeAttribute('open');
    this.dispatchEvent(new Event('close'));
  };
}
