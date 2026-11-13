import {
  ONI_MARK_ANGLED as ANGLED,
  ONI_MARK_FACE_STOPS,
  ONI_MARK_FRONT as FRONT,
  ONI_MARK_INK,
  ONI_MARK_SIDES as SIDES,
  ONI_MARK_VIEW_BOX,
  ONI_MARK_WEIGHTS,
  oniMarkCurvePath,
  oniMarkEyePath,
  oniMarkFrontOutline,
  oniMarkOutlineCurve,
} from "@onicode/shared/oniMark";
import { useIsFocused } from "@react-navigation/native";
import { useEffect, useId, type ComponentProps } from "react";
import { AppState } from "react-native";
import Animated, {
  cancelAnimation,
  Easing,
  useAnimatedProps,
  useAnimatedStyle,
  useDerivedValue,
  useReducedMotion,
  useSharedValue,
  withDelay,
  withSequence,
  withTiming,
  type SharedValue,
} from "react-native-reanimated";
import Svg, { Defs, LinearGradient, Path, Stop, type PathProps } from "react-native-svg";

import { useAppearancePreferences } from "../features/settings/appearance/AppearancePreferencesProvider";

type Side = (typeof SIDES)[number];

const AnimatedPath = Animated.createAnimatedComponent(Path);
type AnimatedPathProps = NonNullable<ComponentProps<typeof AnimatedPath>["animatedProps"]>;

const ASPECT_RATIO = 566 / 500;
const TINY_BELOW = 14;
const weightFor = (height: number) =>
  height < TINY_BELOW ? ONI_MARK_WEIGHTS.tiny : ONI_MARK_WEIGHTS.regular;

const ANGLED_CURVE = oniMarkOutlineCurve(ANGLED.outline);
const OUTLINE = oniMarkCurvePath(ANGLED_CURVE);

interface Motion {
  readonly outline: AnimatedPathProps;
  readonly eyes: Record<Side, AnimatedPathProps>;
}

function Shape({
  motion,
  ...props
}: PathProps & { readonly motion: AnimatedPathProps | undefined }) {
  return motion ? <AnimatedPath {...props} animatedProps={motion} /> : <Path {...props} />;
}

function Mark(props: { readonly height: number; readonly motion?: Motion }) {
  const faceId = `oni-face-${useId().replace(/[^\w-]/g, "")}`;
  const { themeAppearance, themeVariables } = useAppearancePreferences();
  const weight = weightFor(props.height);
  const rim = themeVariables["--color-foreground"];
  return (
    <Svg
      accessibilityLabel="Onicode"
      height={props.height}
      width={props.height * ASPECT_RATIO}
      viewBox={ONI_MARK_VIEW_BOX}
    >
      <Defs>
        <LinearGradient id={faceId} x1="0" y1="1" x2="0" y2="0">
          {ONI_MARK_FACE_STOPS.map((color, i) => (
            <Stop key={color} offset={i} stopColor={color} />
          ))}
        </LinearGradient>
      </Defs>
      {themeAppearance === "light" ? (
        <Shape
          d={OUTLINE}
          motion={props.motion?.outline}
          fill={rim}
          stroke={rim}
          strokeWidth={weight.rim}
          strokeLinejoin="round"
        />
      ) : null}
      <Shape d={OUTLINE} motion={props.motion?.outline} fill={`url(#${faceId})`} />
      {SIDES.map((side) => (
        <Shape
          key={side}
          d={oniMarkEyePath(ANGLED.eyes[side])}
          motion={props.motion?.eyes[side]}
          fill="none"
          stroke={ONI_MARK_INK}
          strokeWidth={weight.eye}
          strokeLinecap="round"
        />
      ))}
    </Svg>
  );
}

/**
 * The mochi oni from the app icon: a blush face with dark eyes. On light it sits inside a rim in
 * the text colour; on dark the rim would only bloat a face that already stands out, so it is
 * dropped. `idle` makes it blink, hop and turn to face front now and then.
 */
export function OniWordmark(props: { readonly height: number; readonly idle?: boolean }) {
  return props.idle ? <IdleMark height={props.height} /> : <Mark height={props.height} />;
}

// The morph blends precomputed curves number by number, so a frame costs one pass over ~400 numbers.
const MAX_LEAN = 28;
const minus = (a: ReadonlyArray<number>, b: ReadonlyArray<number>) => a.map((v, i) => v - b[i]!);
const FRONT_CURVE = oniMarkOutlineCurve(oniMarkFrontOutline());
const TURN_DELTA = minus(FRONT_CURVE, ANGLED_CURVE);
const LEAN_DELTA_LEFT = minus(
  oniMarkOutlineCurve(oniMarkFrontOutline({ left: MAX_LEAN })),
  FRONT_CURVE,
);
const LEAN_DELTA_RIGHT = minus(
  oniMarkOutlineCurve(oniMarkFrontOutline({ right: MAX_LEAN })),
  FRONT_CURVE,
);
const EYE_FROM = { left: [...ANGLED.eyes.left], right: [...ANGLED.eyes.right] };
const EYE_DELTA = {
  left: minus(FRONT.eyes.left, ANGLED.eyes.left),
  right: minus(FRONT.eyes.right, ANGLED.eyes.right),
};

/** `turn` runs 0 (angled) to 1 (front); each lean is a fraction of `MAX_LEAN`. */
function morphedOutline(turn: number, leanLeft: number, leanRight: number) {
  "worklet";
  let path = "";
  for (let i = 0; i < ANGLED_CURVE.length; i += 1) {
    const value =
      ANGLED_CURVE[i]! +
      TURN_DELTA[i]! * turn +
      LEAN_DELTA_LEFT[i]! * leanLeft +
      LEAN_DELTA_RIGHT[i]! * leanRight;
    path += (i === 0 ? "M" : i % 6 === 2 ? "C" : " ") + Math.round(value * 10) / 10;
  }
  return `${path}Z`;
}

/** An eye at `turn`, squashed toward its lower middle as `open` drops from 1. */
function morphedEye(
  from: ReadonlyArray<number>,
  delta: ReadonlyArray<number>,
  turn: number,
  open: number,
  width: number,
) {
  "worklet";
  const x0 = from[0]! + delta[0]! * turn;
  const y0 = from[1]! + delta[1]! * turn;
  const xc = from[2]! + delta[2]! * turn;
  const yc = from[3]! + delta[3]! * turn;
  const x1 = from[4]! + delta[4]! * turn;
  const y1 = from[5]! + delta[5]! * turn;
  const apex = (y0 + 2 * yc + y1) / 4;
  const top = Math.min(y0, y1, apex);
  const pivotY = top + (Math.max(y0, y1, apex) - top) * 0.6;
  const pivotX = (x0 + x1) / 2;
  const wide = 1 + (1 - open) * 0.13;
  const x = (value: number) => Math.round((pivotX + (value - pivotX) * wide) * 10) / 10;
  const y = (value: number) => Math.round((pivotY + (value - pivotY) * open) * 10) / 10;
  return {
    d: `M${x(x0)} ${y(y0)}Q${x(xc)} ${y(yc)} ${x(x1)} ${y(y1)}`,
    strokeWidth: width * (0.6 + 0.4 * Math.min(open, 1)),
  };
}

const TURN_MS = 420;
const TURN_HOLD_MS = 2_160;
const FLAP_AT_MS = 600;
const FLAP_GAP_MS = 240;
const FLAP_SWINGS = [28, -12, 18, -6, 0];
const SWING_MS = 132;
const TURN_EASING = Easing.bezier(0.5, 0, 0.2, 1);

const blinkOnce = () =>
  withSequence(
    withTiming(0.25, { duration: 96, easing: Easing.in(Easing.quad) }),
    withTiming(1.12, { duration: 96, easing: Easing.out(Easing.quad) }),
    withTiming(1, { duration: 48 }),
  );
const flapOnce = () =>
  withSequence(
    ...FLAP_SWINGS.map((degrees) =>
      withTiming(degrees / MAX_LEAN, { duration: SWING_MS, easing: Easing.inOut(Easing.sin) }),
    ),
  );

/**
 * Everything animates on the UI thread from shared values, and only during a move: between moves
 * a plain timer waits, so nothing renders. The timer runs only while the screen is focused, the
 * app is active and reduced motion is off.
 */
function IdleMark(props: { readonly height: number }) {
  const turn = useSharedValue(0);
  const leanLeft = useSharedValue(0);
  const leanRight = useSharedValue(0);
  const openLeft = useSharedValue(1);
  const openRight = useSharedValue(1);
  const lift = useSharedValue(0);
  const squash = useSharedValue(1);
  const eyeWidth = weightFor(props.height).eye;

  const outline = useDerivedValue(() =>
    morphedOutline(turn.value, leanLeft.value, leanRight.value),
  );
  const outlineProps = useAnimatedProps(() => ({ d: outline.value }));
  const leftEye = useAnimatedProps(() =>
    morphedEye(EYE_FROM.left, EYE_DELTA.left, turn.value, openLeft.value, eyeWidth),
  );
  const rightEye = useAnimatedProps(() =>
    morphedEye(EYE_FROM.right, EYE_DELTA.right, turn.value, openRight.value, eyeWidth),
  );
  const hopStyle = useAnimatedStyle(() => ({
    transform: [
      { translateY: lift.value * props.height },
      { scaleX: 1 + (1 - squash.value) * 0.8 },
      { scaleY: squash.value },
    ],
  }));

  const focused = useIsFocused();
  const reducedMotion = useReducedMotion();
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- runs the idle timer while the screen is focused, and settles the mark when it stops
  useEffect(() => {
    if (!focused || reducedMotion) return;
    const eyes = [openLeft, openRight];
    const leans = [leanLeft, leanRight];
    const side = () => (Math.random() < 0.5 ? 0 : 1);
    const blink = (delay: number, only?: number) =>
      eyes.forEach((eye, i) => {
        if (only === undefined || only === i) eye.value = withDelay(delay, blinkOnce());
      });
    const hop = () => {
      squash.value = withSequence(
        withTiming(0.9, { duration: 84, easing: Easing.out(Easing.quad) }),
        withTiming(1.06, { duration: 196, easing: Easing.out(Easing.quad) }),
        withTiming(0.92, { duration: 168, easing: Easing.in(Easing.quad) }),
        withTiming(1, { duration: 112, easing: Easing.out(Easing.quad) }),
      );
      lift.value = withDelay(
        84,
        withSequence(
          withTiming(-0.22, { duration: 196, easing: Easing.out(Easing.quad) }),
          withTiming(0, { duration: 168, easing: Easing.in(Easing.quad) }),
        ),
      );
      blink(420);
    };
    const faceFront = () => {
      turn.value = withSequence(
        withTiming(1, { duration: TURN_MS, easing: TURN_EASING }),
        withDelay(TURN_HOLD_MS, withTiming(0, { duration: TURN_MS, easing: TURN_EASING })),
      );
      const again = Math.random() < 0.5 ? undefined : side();
      leans.forEach((lean, i) => {
        lean.value = withDelay(
          FLAP_AT_MS,
          again === undefined || again === i
            ? withSequence(flapOnce(), withDelay(FLAP_GAP_MS, flapOnce()))
            : flapOnce(),
        );
      });
      if (Math.random() < 0.5) blink(2_280);
    };
    const moves = [
      { weight: 30, run: () => blink(0) },
      {
        weight: 15,
        run: () =>
          eyes.forEach((eye) => {
            eye.value = withSequence(blinkOnce(), withDelay(80, blinkOnce()));
          }),
      },
      { weight: 10, run: () => blink(0, side()) },
      { weight: 15, run: hop },
      { weight: 20, run: faceFront },
    ];
    const totalWeight = moves.reduce((sum, move) => sum + move.weight, 0);

    let timer: ReturnType<typeof setTimeout>;
    const act = () => {
      if (AppState.currentState === "active") {
        let pick = Math.random() * totalWeight;
        const move = moves.find((candidate) => (pick -= candidate.weight) < 0) ?? moves[0]!;
        move.run();
      }
      timer = setTimeout(act, 4_000 + Math.random() * 6_000);
    };
    timer = setTimeout(act, 4_000 + Math.random() * 6_000);

    return () => {
      clearTimeout(timer);
      const rest: ReadonlyArray<readonly [SharedValue<number>, number]> = [
        [turn, 0],
        [leanLeft, 0],
        [leanRight, 0],
        [openLeft, 1],
        [openRight, 1],
        [lift, 0],
        [squash, 1],
      ];
      for (const [value, at] of rest) {
        cancelAnimation(value);
        value.value = at;
      }
    };
  }, [focused, reducedMotion, turn, leanLeft, leanRight, openLeft, openRight, lift, squash]);

  return (
    <Animated.View style={[{ transformOrigin: "50% 100%" }, hopStyle]}>
      <Mark
        height={props.height}
        motion={{ outline: outlineProps, eyes: { left: leftEye, right: rightEye } }}
      />
    </Animated.View>
  );
}
