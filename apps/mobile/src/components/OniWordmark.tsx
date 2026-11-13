import {
  ONI_MARK_ANGLED,
  ONI_MARK_FACE_STOPS,
  ONI_MARK_INK,
  ONI_MARK_SIDES,
  ONI_MARK_VIEW_BOX,
  ONI_MARK_WEIGHTS,
  oniMarkEyePath,
  oniMarkOutlinePath,
} from "@onicode/shared/oniMark";
import { useId } from "react";
import Svg, { Defs, LinearGradient, Path, Stop } from "react-native-svg";
import { withUniwind } from "uniwind";

import { useAppearancePreferences } from "../features/settings/appearance/AppearancePreferencesProvider";

const ThemedPath = withUniwind(Path);
const ASPECT_RATIO = 566 / 500;
const OUTLINE = oniMarkOutlinePath(ONI_MARK_ANGLED.outline);
const TINY_BELOW = 14;

/**
 * The mochi oni from the app icon: a blush face with dark eyes. On light it sits inside a rim in
 * the text colour; on dark the rim would only bloat a face that already stands out, so it is dropped.
 */
export function OniWordmark(props: { readonly height: number }) {
  const faceId = `oni-face-${useId().replace(/[^\w-]/g, "")}`;
  const light = useAppearancePreferences().themeAppearance === "light";
  const weight = props.height < TINY_BELOW ? ONI_MARK_WEIGHTS.tiny : ONI_MARK_WEIGHTS.regular;
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
      {light ? (
        <ThemedPath
          d={OUTLINE}
          colorClassName="accent-foreground"
          fill="currentColor"
          stroke="currentColor"
          strokeWidth={weight.rim}
          strokeLinejoin="round"
        />
      ) : null}
      <Path d={OUTLINE} fill={`url(#${faceId})`} />
      {ONI_MARK_SIDES.map((side) => (
        <Path
          key={side}
          d={oniMarkEyePath(ONI_MARK_ANGLED.eyes[side])}
          fill="none"
          stroke={ONI_MARK_INK}
          strokeWidth={weight.eye}
          strokeLinecap="round"
        />
      ))}
    </Svg>
  );
}
