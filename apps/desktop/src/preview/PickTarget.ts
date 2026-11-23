import type { PreviewAnnotationRect } from "@onicode/contracts";

interface PickScope<E> {
  elementsFromPoint(x: number, y: number): ReadonlyArray<E>;
  contains(node: NoInfer<E>): boolean;
}

// Document hit-testing retargets shadow content to its host, so descend into open roots.
// ShadowRoot.elementsFromPoint still lists the host and the rest of the page (the overlay
// too), so only the root's own elements count. Closed roots expose no shadowRoot.
export function pickElementAtPoint<E extends { readonly shadowRoot: PickScope<E> | null }>(
  scope: PickScope<E>,
  x: number,
  y: number,
  isExcluded: (element: E) => boolean,
): E | null {
  for (const candidate of scope.elementsFromPoint(x, y)) {
    if (!scope.contains(candidate) || isExcluded(candidate)) continue;
    const inner =
      candidate.shadowRoot && pickElementAtPoint(candidate.shadowRoot, x, y, isExcluded);
    return inner ?? candidate;
  }
  return null;
}

/** Every element below `root`, including those inside open shadow roots, as Select can pick them. */
export function deepElements(root: ParentNode): Element[] {
  const elements: Element[] = [];
  const visit = (scope: ParentNode) => {
    for (const element of scope.querySelectorAll("*")) {
      elements.push(element);
      if (element.shadowRoot) visit(element.shadowRoot);
    }
  };
  visit(root);
  return elements;
}

// Layout follows the flat tree: slotted content sits in its slot, a shadow root's top in its host.
function flatTreeParent(element: Element): Element | null {
  if (element.assignedSlot) return element.assignedSlot;
  if (element.parentElement) return element.parentElement;
  const root = element.getRootNode();
  return root instanceof ShadowRoot ? root.host : null;
}

export function visibleElementRect(element: Element): PreviewAnnotationRect {
  const rect = element.getBoundingClientRect();
  let left = Math.max(0, rect.left);
  let top = Math.max(0, rect.top);
  let right = Math.min(document.documentElement.clientWidth, rect.right);
  let bottom = Math.min(document.documentElement.clientHeight, rect.bottom);
  if (getComputedStyle(element).position !== "fixed") {
    for (let ancestor = flatTreeParent(element); ancestor; ancestor = flatTreeParent(ancestor)) {
      if (ancestor === document.body || ancestor === document.documentElement) break;
      const style = getComputedStyle(ancestor);
      const bounds = ancestor.getBoundingClientRect();
      const clipLeft = bounds.left + ancestor.clientLeft;
      const clipTop = bounds.top + ancestor.clientTop;
      if (style.overflowX !== "visible") {
        left = Math.max(left, clipLeft);
        right = Math.min(right, clipLeft + ancestor.clientWidth);
      }
      if (style.overflowY !== "visible") {
        top = Math.max(top, clipTop);
        bottom = Math.min(bottom, clipTop + ancestor.clientHeight);
      }
    }
  }
  return { x: left, y: top, width: Math.max(0, right - left), height: Math.max(0, bottom - top) };
}
