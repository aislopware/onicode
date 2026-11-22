/**
 * Narrows `content`'s parent, a shrink-to-fit box, to the widest line of text inside `content`.
 * Text that wraps leaves the box at its max width, so balanced lines end well short of it and
 * leave empty space on one side.
 */
export function fitBoxToText(content: HTMLElement): void {
  const box = content.parentElement;
  if (!box) return;
  box.style.removeProperty("width");
  const outer = content.getBoundingClientRect();
  if (content.offsetWidth === 0) return;
  // A popup mid scale-in reports scaled rects; layout widths are unscaled.
  const scale = outer.width / content.offsetWidth;
  const style = getComputedStyle(content);
  const start = outer.left + Number.parseFloat(style.paddingLeft) * scale;
  const end = outer.right - Number.parseFloat(style.paddingRight) * scale;
  let left = end;
  let right = start;
  const extend = (rect: DOMRect) => {
    if (rect.width === 0) return;
    left = Math.min(left, rect.left);
    right = Math.max(right, rect.right);
  };
  const range = document.createRange();
  const walker = document.createTreeWalker(content, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) {
    range.selectNodeContents(walker.currentNode);
    for (const rect of range.getClientRects()) extend(rect);
  }
  for (const element of content.querySelectorAll("kbd, svg, img")) {
    extend(element.getBoundingClientRect());
  }
  if (right <= left) return;
  const slack = (end - right + (left - start)) / scale;
  if (slack < 1) return;
  box.style.width = `${Math.ceil(box.getBoundingClientRect().width / scale - slack)}px`;
}
