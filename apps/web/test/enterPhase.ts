/**
 * Records whether a popup matching `selector` ever showed Base UI's enter phase. A root that mounts
 * already open skips it, and its popup then slides in from the window's top-left corner.
 */
export function watchEnterPhase(selector: string) {
  let entered = false;
  const observer = new MutationObserver(() => {
    entered ||= document.querySelector(`${selector}[data-starting-style]`) !== null;
  });
  observer.observe(document.body, { subtree: true, childList: true, attributes: true });
  return {
    get entered() {
      return entered;
    },
    [Symbol.dispose]: () => observer.disconnect(),
  };
}
