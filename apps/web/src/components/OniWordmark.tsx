import { useId, type SVGProps } from "react";

const BODY =
  "M391 192C420 192 446 194 466 199C500 207 530 218 555.4 233.9C596.6 260.7 627.2 296.6 644.7 338.9C660.6 377.1 664.1 418 654.7 456.7C645.4 495.4 625 527.7 592.9 554.3C544.4 594.7 468.4 618 391 619" +
  "C313.6 618 237.6 594.7 189.1 554.3C157 527.7 136.6 495.4 127.3 456.7C117.9 418 121.4 377.1 137.3 338.9C154.8 296.6 185.4 260.7 226.6 233.9C252 218 282 207 316 199C336 194 362 192 391 192Z";
const EARS = {
  left: "M316 199C307.8 200.6 305.7 199.6 297.2 190.8C289.5 182.5 284.1 173.1 278.6 158.3C274.2 146.3 271.8 143.2 266.1 141.8C255.2 139.3 243.2 155.3 237.7 179.6C234.4 194 234 204.7 236.2 222.4C236.8 227.1 236.4 227.5 226.6 233.9L242 252L306 236Z",
  right: "M466 199C474.2 200.6 476.3 199.6 484.8 190.8C492.5 182.5 497.9 173.1 503.4 158.3C507.8 146.3 510.2 143.2 515.9 141.8C526.8 139.3 538.8 155.3 544.3 179.6C547.6 194 548 204.7 545.8 222.4C545.2 227.1 545.6 227.5 555.4 233.9L540 252L476 236Z",
};
const EYES = {
  left: "M227.2 378.4Q276.9 344.8 326.6 378.4",
  right: "M455.4 378.4Q505.1 344.8 554.8 378.4",
};
const SIDES = ["left", "right"] as const;

/** The mochi oni, front-on, in one ink (`currentColor`); its eyes are holes, so the background shows through. */
export function OniWordmark(props: SVGProps<SVGSVGElement>) {
  const maskId = `oni-eyes-${useId().replace(/[^\w-]/g, "")}`;
  return (
    <svg {...props} viewBox="108 128 566 500" overflow="visible" xmlns="http://www.w3.org/2000/svg">
      <mask id={maskId} maskUnits="userSpaceOnUse" x="0" y="0" width="800" height="800">
        <rect width="800" height="800" fill="#fff" />
        {SIDES.map((side) => (
          <path
            key={side}
            data-oni-eye={side}
            d={EYES[side]}
            fill="none"
            stroke="#000"
            strokeWidth="38"
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
export function animateOniIdle(root: Element) {
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
