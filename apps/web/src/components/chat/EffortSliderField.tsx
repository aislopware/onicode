import type { CSSProperties } from "react";

import { DitherField } from "../DitherField";

/** Hue, in degrees from the accent, at the fast end and at the smart end. */
const HUE_FAST = -50;
const HUE_SMART = 64;
/** The fill's width at the first level: the thumb and the gap around it. */
const FILL_MIN = 20;

/**
 * Where a level sits on the hue range: 0 is the fast end, 1 the smart end, and the default level
 * is 0.5, the accent itself, whenever it is not at an end.
 */
export function effortTone(index: number, last: number, defaultIndex: number): number {
  if (last <= 0) return 0.5;
  if (defaultIndex <= 0 || defaultIndex >= last) return Math.max(0, index) / last;
  if (index <= defaultIndex) return (0.5 * Math.max(0, index)) / defaultIndex;
  return 0.5 + (0.5 * (index - defaultIndex)) / (last - defaultIndex);
}

/** A tone's hue, in degrees from the accent. */
export function effortHue(tone: number): number {
  return tone < 0.5 ? HUE_FAST * (1 - 2 * tone) : HUE_SMART * (2 * tone - 1);
}

/**
 * The lit part of the effort slider: a fill in the level's hue from the left edge to just past the
 * thumb, with squares that thicken toward the thumb. The squares are redrawn as the fill's width
 * glides to a new level, so they flow with it.
 */
export function EffortSliderField(props: {
  /** The shown level and the top one, both as indexes. */
  level: number;
  top: number;
  /** The shown level's place on the hue range, from effortTone. */
  tone: number;
}) {
  const reach = props.top > 0 ? props.level / props.top : 0;
  return (
    <span
      aria-hidden="true"
      className="effort-slider-fill pointer-events-none absolute inset-y-0 left-0 overflow-hidden rounded-lg transition-[width,background-color] duration-200 ease-out motion-reduce:transition-none"
      style={
        {
          width: `calc(${FILL_MIN}px + (100% - ${FILL_MIN}px) * ${reach})`,
          "--effort-level-hue": effortHue(props.tone),
        } as CSSProperties
      }
    >
      <span className="absolute inset-0 bg-linear-to-r from-black/30 to-transparent" />
      <DitherField
        from={0}
        to={0.5}
        className="inset-y-0 left-0 h-full w-[calc(100%-22px)] text-white opacity-45"
      />
    </span>
  );
}
