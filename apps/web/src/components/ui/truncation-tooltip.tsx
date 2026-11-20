import { useEffect, useState } from "react";

import { cn } from "~/lib/utils";

import { Tooltip, TooltipPopup, TOOLTIP_DELAY_MS, TOOLTIP_WARM_MS } from "./tooltip";

const TRUNCATING = ".truncate, [class*='line-clamp-']";
// An element that already explains itself on hover keeps the pointer to itself.
const OWN_POPUP = "[data-slot='tooltip-trigger'], [data-popup-open]";
// A native title shows on hover only, so it explains nothing to the keyboard.
const SELF_EXPLAINING = `${OWN_POPUP}, [title]:not([title=''])`;
const OPEN_DELAY_MS = TOOLTIP_DELAY_MS;
const MAX_TEXT_LENGTH = 1200;

function isCutOff(element: HTMLElement): boolean {
  if (element.clientWidth === 0) return false;
  if (!element.matches("[class*='line-clamp-']")) return element.scrollWidth > element.clientWidth;
  // A clamped block that opens in place already shows several lines; a tooltip would cover them.
  return (
    element.scrollHeight > element.clientHeight + 1 && element.closest("[aria-expanded]") === null
  );
}

/** The cut-off text a focused control holds: the control itself, or the first such label inside it. */
function findCutOffWithin(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof HTMLElement) || !target.matches(":focus-visible")) return null;
  if (target.closest(OWN_POPUP)) return null;
  const candidates = [target, ...target.querySelectorAll<HTMLElement>(TRUNCATING)];
  return candidates.find((element) => element.matches(TRUNCATING) && isCutOff(element)) ?? null;
}

function findCutOff(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof Element)) return null;
  let element = target.closest<HTMLElement>(TRUNCATING);
  while (element) {
    if (isCutOff(element)) return element.closest(SELF_EXPLAINING) ? null : element;
    element = element.parentElement?.closest<HTMLElement>(TRUNCATING) ?? null;
  }
  return null;
}

/** The text a sighted reader would see uncut: screen-reader-only and hidden parts stay out. */
export function visibleText(element: HTMLElement): string {
  const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT, (node) =>
    node.parentElement?.closest(".sr-only, [aria-hidden='true']")
      ? NodeFilter.FILTER_REJECT
      : NodeFilter.FILTER_ACCEPT,
  );
  let text = "";
  while (walker.nextNode()) text += walker.currentNode.nodeValue;
  const collapsed = text.replace(/[^\S\n]+/gu, " ").trim();
  return collapsed.length > MAX_TEXT_LENGTH
    ? `${collapsed.slice(0, MAX_TEXT_LENGTH).trimEnd()}…`
    : collapsed;
}

interface CutOffTip {
  readonly id: number;
  readonly text: string;
  readonly code: boolean;
  readonly instant: boolean;
  readonly anchor: { getBoundingClientRect: () => DOMRect };
}

/**
 * One tooltip for every cut-off label in the app: resting the pointer on text that ends in an
 * ellipsis shows it in full. Mounted once, so rows pay nothing for it.
 */
export function TruncationTooltip() {
  const [tip, setTip] = useState<CutOffTip | null>(null);
  const [open, setOpen] = useState(false);

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- one set of document pointer listeners finds cut-off text anywhere in the app
  useEffect(() => {
    let hovered: HTMLElement | null = null;
    let pointerX = 0;
    let timer: number | undefined;
    let closedAt = Number.NEGATIVE_INFINITY;
    let nextId = 0;

    const close = () => {
      window.clearTimeout(timer);
      if (hovered) closedAt = performance.now();
      hovered = null;
      setOpen(false);
    };
    const show = (element: HTMLElement, instant: boolean, atPointer = true) => {
      const text = visibleText(element);
      if (!text || !element.isConnected || !isCutOff(element)) return;
      const rect = element.getBoundingClientRect();
      // Above the text, at the pointer: a wide row would otherwise put it far from the eye.
      const x = atPointer ? Math.min(rect.right, Math.max(rect.left, pointerX)) : rect.left;
      nextId += 1;
      setTip({
        id: nextId,
        text,
        code: /mono/iu.test(getComputedStyle(element).fontFamily),
        instant,
        anchor: {
          getBoundingClientRect: () =>
            new DOMRect(x, rect.top, atPointer ? 0 : rect.width, rect.height),
        },
      });
      setOpen(true);
    };
    const onPointerOver = (event: PointerEvent) => {
      if (event.pointerType !== "mouse") return;
      pointerX = event.clientX;
      const element = event.buttons === 0 ? findCutOff(event.target) : null;
      if (element === hovered) return;
      const warm = hovered !== null || performance.now() - closedAt < TOOLTIP_WARM_MS;
      close();
      if (!element) return;
      hovered = element;
      if (warm) show(element, true);
      else timer = window.setTimeout(() => show(element, false), OPEN_DELAY_MS);
    };
    const onPointerMove = (event: PointerEvent) => {
      pointerX = event.clientX;
    };
    const onPointerOut = (event: PointerEvent) => {
      if (event.relatedTarget === null) close();
    };
    // Keyboard users reach the full text too: focus moved by Tab shows it at once.
    const onFocusIn = (event: FocusEvent) => {
      const element = findCutOffWithin(event.target);
      close();
      if (!element) return;
      hovered = element;
      show(element, true, false);
    };

    const options = { capture: true, passive: true } as const;
    document.addEventListener("pointerover", onPointerOver, options);
    document.addEventListener("pointermove", onPointerMove, options);
    document.addEventListener("pointerout", onPointerOut, options);
    document.addEventListener("pointerdown", close, options);
    document.addEventListener("focusin", onFocusIn, options);
    document.addEventListener("focusout", close, options);
    document.addEventListener("keydown", close, options);
    document.addEventListener("scroll", close, options);
    window.addEventListener("blur", close);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener("pointerover", onPointerOver, options);
      document.removeEventListener("pointermove", onPointerMove, options);
      document.removeEventListener("pointerout", onPointerOut, options);
      document.removeEventListener("pointerdown", close, options);
      document.removeEventListener("focusin", onFocusIn, options);
      document.removeEventListener("focusout", close, options);
      document.removeEventListener("keydown", close, options);
      document.removeEventListener("scroll", close, options);
      window.removeEventListener("blur", close);
    };
  }, []);

  if (!tip) return null;
  return (
    <Tooltip key={tip.id} open={open} onOpenChange={setOpen}>
      <TooltipPopup
        anchor={tip.anchor}
        side="top"
        variant={tip.code ? "code" : "default"}
        className={cn(
          tip.instant && "duration-0",
          tip.text.length > 80 && "font-normal leading-normal text-pretty",
        )}
        data-truncation-tooltip
      >
        <span className="line-clamp-[12] whitespace-pre-line">{tip.text}</span>
      </TooltipPopup>
    </Tooltip>
  );
}
