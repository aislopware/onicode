import type { KeyboardEvent } from "react";

/** Arrow keys move between a chart's bars, so the chart is one Tab stop: its first bar. */
export function moveChartBarFocus(event: KeyboardEvent<HTMLElement>) {
  const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
  if (step === 0 || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
  const bars = Array.from(event.currentTarget.children);
  const current = bars.findIndex((bar) => bar === event.target);
  const next = current === -1 ? undefined : bars[current + step];
  if (!(next instanceof HTMLElement)) return;
  event.preventDefault();
  next.focus();
}
