import {
  ONI_MARK_ANGLED as ANGLED,
  ONI_MARK_FACE_STOPS,
  ONI_MARK_INK,
  ONI_MARK_SIDES as SIDES,
  ONI_MARK_VIEW_BOX,
  ONI_MARK_WEIGHTS,
  oniMarkCurvePath,
  oniMarkEyePath,
  oniMarkOutlineCurve,
} from "@onicode/shared/oniMark";
import { useId } from "react";
import Svg, { Defs, LinearGradient, Path, Stop } from "react-native-svg";

import { useAppearancePreferences } from "../features/settings/appearance/AppearancePreferencesProvider";

const ASPECT_RATIO = 566 / 500;
const TINY_BELOW = 14;
const weightFor = (height: number) =>
  height < TINY_BELOW ? ONI_MARK_WEIGHTS.tiny : ONI_MARK_WEIGHTS.regular;

const OUTLINE = oniMarkCurvePath(oniMarkOutlineCurve(ANGLED.outline));

/**
 * The mochi oni from the app icon: a blush face with dark eyes. On light it sits inside a rim in
 * the text colour; on dark the rim would only bloat a face that already stands out, so it is
 * dropped. It stays still on mobile: a moving mark costs a commit and a path redraw per frame.
 */
export function OniWordmark(props: { readonly height: number }) {
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
        <Path d={OUTLINE} fill={rim} stroke={rim} strokeWidth={weight.rim} strokeLinejoin="round" />
      ) : null}
      <Path d={OUTLINE} fill={`url(#${faceId})`} />
      {SIDES.map((side) => (
        <Path
          key={side}
          d={oniMarkEyePath(ANGLED.eyes[side])}
          fill="none"
          stroke={ONI_MARK_INK}
          strokeWidth={weight.eye}
          strokeLinecap="round"
        />
      ))}
    </Svg>
  );
}
