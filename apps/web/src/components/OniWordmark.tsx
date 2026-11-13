import {
  ONI_MARK_ANGLED as ANGLED,
  ONI_MARK_FACE_STOPS,
  ONI_MARK_FRONT as FRONT,
  ONI_MARK_INK as INK,
  ONI_MARK_SIDES as SIDES,
  ONI_MARK_VIEW_BOX,
  ONI_MARK_WEIGHTS,
  oniMarkEyePath,
  oniMarkFrontOutline,
  oniMarkOutlinePath,
} from "@onicode/shared/oniMark";
import { useId, type SVGProps } from "react";

const ANGLED_OUTLINE = oniMarkOutlinePath(ANGLED.outline);
const BODY_STYLE = { transformBox: "fill-box", transformOrigin: "50% 100%" } as const;
const EYE_STYLE = { transformBox: "fill-box", transformOrigin: "50% 60%" } as const;

/**
 * The mochi oni from the app icon: a blush face with dark eyes. On light it sits inside a rim in
 * the text colour; on dark, and over media (`onBackdrop`), the rim would only bloat a face that
 * already stands out, so it is dropped. `tiny` thickens rim and eyes for marks under 14px.
 */
export function OniWordmark({
  tiny = false,
  onBackdrop = false,
  ...props
}: SVGProps<SVGSVGElement> & { tiny?: boolean; onBackdrop?: boolean }) {
  const faceId = `oni-face-${useId().replace(/[^\w-]/g, "")}`;
  const weight = tiny ? ONI_MARK_WEIGHTS.tiny : ONI_MARK_WEIGHTS.regular;
  return (
    <svg
      {...props}
      viewBox={ONI_MARK_VIEW_BOX}
      overflow="visible"
      xmlns="http://www.w3.org/2000/svg"
    >
      <defs>
        <linearGradient id={faceId} gradientUnits="userSpaceOnUse" x1="0" y1="619" x2="0" y2="142">
          {ONI_MARK_FACE_STOPS.map((color, i) => (
            <stop key={color} offset={i} stopColor={color} />
          ))}
        </linearGradient>
      </defs>
      <g data-oni-body="" style={BODY_STYLE}>
        {onBackdrop ? null : (
          <path
            data-oni-outline=""
            className="text-foreground dark:hidden"
            d={ANGLED_OUTLINE}
            fill="currentColor"
            stroke="currentColor"
            strokeWidth={weight.rim}
            strokeLinejoin="round"
          />
        )}
        <path data-oni-outline="" d={ANGLED_OUTLINE} fill={`url(#${faceId})`} stroke="none" />
        <g fill="none" stroke={INK} strokeWidth={weight.eye} strokeLinecap="round">
          {SIDES.map((side) => (
            <path
              key={side}
              data-oni-eye={side}
              d={oniMarkEyePath(ANGLED.eyes[side])}
              style={EYE_STYLE}
            />
          ))}
        </g>
      </g>
    </svg>
  );
}

const BLINK: Keyframe[] = [
  { transform: "scale(1, 1)", easing: "ease-in" },
  { transform: "scale(1.1, 0.25)", offset: 0.4, easing: "ease-out" },
  { transform: "scale(0.97, 1.12)", offset: 0.8, easing: "ease-in-out" },
  { transform: "scale(1, 1)" },
];

const HOP: Keyframe[] = [
  { transform: "translateY(0) scale(1, 1)", easing: "ease-out" },
  { transform: "translateY(0) scale(1.08, 0.9)", offset: 0.15, easing: "ease-out" },
  { transform: "translateY(-28%) scale(0.95, 1.06)", offset: 0.5, easing: "ease-in" },
  { transform: "translateY(0) scale(1.07, 0.92)", offset: 0.8, easing: "ease-out" },
  { transform: "translateY(0) scale(1, 1)" },
];

type Side = (typeof SIDES)[number];
type Lean = Partial<Record<Side, number>>;

const frontOutline = (lean?: Lean) => oniMarkOutlinePath(oniMarkFrontOutline(lean));

const cssPath = (path: string) => `path("${path}")`;
const SWING = "cubic-bezier(0.37, 0, 0.63, 1)";
const TURN_MS = 3_000;
const TURN = 0.14;
const FLAP_SWINGS = [28, -12, 18, -6, 0];
const FLAP_SPAN = 0.22;

interface Flap {
  readonly at: number;
  readonly sides: ReadonlyArray<Side>;
}

/** One pass of a turn: morph to the front pose, flap, hold, and morph back. */
function turnKeyframes(flaps: ReadonlyArray<Flap>) {
  const rest = cssPath(ANGLED_OUTLINE);
  const front = cssPath(frontOutline());
  const outline: Keyframe[] = [
    { d: rest, easing: "cubic-bezier(0.5, 0, 0.2, 1)" },
    { d: front, offset: TURN, easing: SWING },
  ];
  for (const flap of flaps) {
    outline.push({ d: front, offset: flap.at, easing: SWING });
    FLAP_SWINGS.forEach((degrees, i) => {
      const lean: Lean = {};
      for (const side of flap.sides) lean[side] = degrees;
      outline.push({
        d: cssPath(frontOutline(lean)),
        offset: flap.at + (FLAP_SPAN * (i + 1)) / FLAP_SWINGS.length,
        easing: SWING,
      });
    });
  }
  outline.push({ d: front, offset: 1 - TURN, easing: "cubic-bezier(0.5, 0, 0.2, 1)" }, { d: rest });
  const eye = (side: Side): Keyframe[] => {
    const turned = cssPath(oniMarkEyePath(FRONT.eyes[side]));
    return [
      { d: cssPath(oniMarkEyePath(ANGLED.eyes[side])), easing: "cubic-bezier(0.5, 0, 0.2, 1)" },
      { d: turned, offset: TURN },
      { d: turned, offset: 1 - TURN, easing: "cubic-bezier(0.5, 0, 0.2, 1)" },
      { d: cssPath(oniMarkEyePath(ANGLED.eyes[side])) },
    ];
  };
  return { outline, eyes: SIDES.map(eye) };
}

/**
 * Ref callback for an element containing an `OniWordmark`: now and then it blinks, winks, hops, or
 * turns to face front and flaps its ears. Idle time is a plain timer so nothing animates between
 * moves.
 */
export function animateOniIdle(root: Element | null) {
  // React calls a ref with null on detach when it returned no cleanup.
  if (!root) return;
  const body = root.querySelector("[data-oni-body]");
  const outlines = [...root.querySelectorAll("[data-oni-outline]")];
  const eyes = SIDES.map((side) => root.querySelector(`[data-oni-eye="${side}"]`));
  if (!body || outlines.length === 0 || eyes.includes(null)) return;
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  let timer: ReturnType<typeof setTimeout>;

  const blink = (delay = 0, only?: number) =>
    eyes.forEach((eye, i) => {
      if (only === undefined || only === i) eye?.animate(BLINK, { duration: 240, delay });
    });
  const side = () => (Math.random() < 0.5 ? 0 : 1);
  const faceFront = () => {
    const second: Flap = { at: 0.5, sides: Math.random() < 0.5 ? SIDES : [SIDES[side()]!] };
    const turn = turnKeyframes([{ at: 0.2, sides: SIDES }, second]);
    for (const outline of outlines) outline.animate(turn.outline, { duration: TURN_MS });
    eyes.forEach((eye, i) => eye?.animate(turn.eyes[i]!, { duration: TURN_MS }));
    if (Math.random() < 0.5) blink(TURN_MS * 0.76);
  };

  const moves = [
    { weight: 30, run: () => blink() },
    { weight: 15, run: () => (blink(), blink(320)) },
    { weight: 10, run: () => blink(0, side()) },
    { weight: 15, run: () => (body.animate(HOP, { duration: 560 }), blink(420)) },
    { weight: 20, run: faceFront },
  ];
  const totalWeight = moves.reduce((sum, move) => sum + move.weight, 0);

  const act = () => {
    if (!document.hidden && !reducedMotion.matches) {
      let pick = Math.random() * totalWeight;
      const move = moves.find((candidate) => (pick -= candidate.weight) < 0) ?? moves[0]!;
      move.run();
    }
    timer = setTimeout(act, 4_000 + Math.random() * 6_000);
  };

  timer = setTimeout(act, 4_000 + Math.random() * 6_000);
  return () => clearTimeout(timer);
}
