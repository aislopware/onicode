import {
  ONI_MARK_BODY as BODY,
  ONI_MARK_EARS as EARS,
  ONI_MARK_EYE_STROKE_WIDTH,
  ONI_MARK_EYES as EYES,
  ONI_MARK_SIDES as SIDES,
  ONI_MARK_VIEW_BOX,
} from "@onicode/shared/oniMark";
import { useId, type SVGProps } from "react";

/** The mochi oni, front-on, in one ink (`currentColor`); its eyes are holes, so the background shows through. */
export function OniWordmark(props: SVGProps<SVGSVGElement>) {
  const maskId = `oni-eyes-${useId().replace(/[^\w-]/g, "")}`;
  return (
    <svg
      {...props}
      viewBox={ONI_MARK_VIEW_BOX}
      overflow="visible"
      xmlns="http://www.w3.org/2000/svg"
    >
      <mask id={maskId} maskUnits="userSpaceOnUse" x="0" y="0" width="800" height="800">
        {/* oxlint-disable-next-line shadcn/no-raw-colors -- mask luminance */}
        <rect width="800" height="800" fill="#fff" />
        {SIDES.map((side) => (
          <path
            key={side}
            data-oni-eye={side}
            d={EYES[side]}
            fill="none"
            // oxlint-disable-next-line shadcn/no-raw-colors -- mask luminance
            stroke="#000"
            strokeWidth={ONI_MARK_EYE_STROKE_WIDTH}
            strokeLinecap="round"
            style={{ transformBox: "fill-box", transformOrigin: "50% 60%" }}
          />
        ))}
      </mask>
      <g
        data-oni-body=""
        fill="currentColor"
        style={{ transformBox: "fill-box", transformOrigin: "50% 100%" }}
      >
        {SIDES.map((side) => (
          <path
            key={side}
            data-oni-ear={side}
            d={EARS[side]}
            style={{ transformBox: "fill-box", transformOrigin: "50% 70%" }}
          />
        ))}
        <path d={BODY} mask={`url(#${maskId})`} />
      </g>
    </svg>
  );
}

const BLINK: Keyframe[] = [
  { transform: "scale(1, 1)" },
  { transform: "scale(1.12, 0.3)", offset: 0.4 },
  { transform: "scale(1.12, 0.3)", offset: 0.55 },
  { transform: "scale(0.96, 1.2)", offset: 0.8 },
  { transform: "scale(1, 1)" },
];

const HOP: Keyframe[] = [
  { transform: "translateY(0) scale(1, 1)", easing: "ease-out" },
  { transform: "translateY(0) scale(1.08, 0.9)", offset: 0.15, easing: "ease-out" },
  { transform: "translateY(-28%) scale(0.95, 1.06)", offset: 0.5, easing: "ease-in" },
  { transform: "translateY(0) scale(1.07, 0.92)", offset: 0.8, easing: "ease-out" },
  { transform: "translateY(0) scale(1, 1)" },
];

// Skew, not rotate: a rotated ear lifts its inner base off the head and opens a gap.
const earFlap = (outward: 1 | -1): Keyframe[] =>
  [0, 18, -8, 12, -4, 0].map((deg) => ({ transform: `skewX(${-deg * outward}deg)` }));
const EAR_FLAP = { left: earFlap(-1), right: earFlap(1) };

/**
 * Ref callback for an element containing an `OniWordmark`: now and then it blinks, winks,
 * flaps its ears or hops. Idle time is a plain timer so nothing animates between moves.
 */
export function animateOniIdle(root: Element | null) {
  // React calls a ref with null on detach when it returned no cleanup.
  if (!root) return;
  const body = root.querySelector("[data-oni-body]");
  const eyes = SIDES.map((side) => root.querySelector(`[data-oni-eye="${side}"]`));
  const ears = SIDES.map((side) => root.querySelector(`[data-oni-ear="${side}"]`));
  if (!body || eyes.includes(null) || ears.includes(null)) return;
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  let timer: ReturnType<typeof setTimeout>;

  const blink = (delay = 0, only?: number) =>
    eyes.forEach((eye, i) => {
      if (only === undefined || only === i) eye?.animate(BLINK, { duration: 220, delay });
    });
  const flap = (delay = 0, only?: number) =>
    ears.forEach((ear, i) => {
      if (only === undefined || only === i)
        ear?.animate(EAR_FLAP[SIDES[i]!], { duration: 520, delay, easing: "ease-in-out" });
    });
  const side = () => (Math.random() < 0.5 ? 0 : 1);

  const moves = [
    { weight: 30, run: () => blink() },
    { weight: 15, run: () => (blink(), blink(300)) },
    { weight: 10, run: () => blink(0, side()) },
    { weight: 15, run: () => flap() },
    { weight: 10, run: () => flap(0, side()) },
    { weight: 20, run: () => (body.animate(HOP, { duration: 560 }), flap(200), blink(420)) },
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
