import { useEffect, useRef } from "react";

import { observeResize } from "~/lib/observeResize";
import { cn } from "~/lib/utils";

/** A 4×4 ordered-dither (Bayer) matrix: the order in which a block's squares fill in. */
const FILL_ORDER = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/** Whether the square at a column and row is drawn where `density` of the squares should be. */
function ditherSquareLit(column: number, row: number, density: number): boolean {
  return density > ((FILL_ORDER[(row % 4) * 4 + (column % 4)] ?? 0) + 0.5) / 16;
}

/**
 * How dense the squares are at a place in the field, both coordinates from 0 to 1. Density eases
 * from `from` to `to` along the width, starting at `start`. A `slant` tilts that direction toward
 * the bottom edge, or toward the top edge when negative.
 */
function ditherDensity(
  along: number,
  down: number,
  ramp: { from: number; to: number; start: number; slant: number },
): number {
  const tilt = Math.abs(ramp.slant);
  const place = (1 - tilt) * along + tilt * (ramp.slant < 0 ? 1 - down : down);
  const eased = Math.min(1, Math.max(0, (place - ramp.start) / (1 - ramp.start)));
  return ramp.from + (ramp.to - ramp.from) * eased * eased * (3 - 2 * eased);
}

/**
 * A field of small squares that thins out across its width, in the element's text color. It is
 * drawn once per size and theme, one canvas pixel per square, and scaled up unsmoothed.
 */
export function DitherField(props: {
  from: number;
  to: number;
  start?: number;
  slant?: number;
  /** The square's side in CSS pixels. */
  cell?: number;
  className?: string;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const { from, to, start = 0, slant = 0, cell = 2 } = props;

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- draws on a canvas, again when its box or the theme's colors change
  useEffect(() => {
    const canvas = canvasRef.current;
    const context = canvas?.getContext("2d");
    if (!canvas || !context) return;
    const draw = () => {
      const columns = Math.ceil(canvas.offsetWidth / cell);
      const rows = Math.ceil(canvas.offsetHeight / cell);
      if (columns === 0 || rows === 0) return;
      canvas.width = columns;
      canvas.height = rows;
      context.fillStyle = getComputedStyle(canvas).color;
      for (let row = 0; row < rows; row++) {
        for (let column = 0; column < columns; column++) {
          const density = ditherDensity((column + 0.5) / columns, (row + 0.5) / rows, {
            from,
            to,
            start,
            slant,
          });
          if (ditherSquareLit(column, row, density)) context.fillRect(column, row, 1, 1);
        }
      }
    };
    draw();
    const stopObserving = observeResize(canvas, draw);
    const themeObserver = new MutationObserver(draw);
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class", "style"],
    });
    return () => {
      stopObserving();
      themeObserver.disconnect();
    };
  }, [from, to, start, slant, cell]);

  return (
    <canvas
      ref={canvasRef}
      aria-hidden="true"
      className={cn("pointer-events-none absolute [image-rendering:pixelated]", props.className)}
    />
  );
}
