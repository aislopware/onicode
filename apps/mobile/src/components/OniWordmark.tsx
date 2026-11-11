import {
  ONI_MARK_BODY,
  ONI_MARK_EARS,
  ONI_MARK_EYE_STROKE_WIDTH,
  ONI_MARK_EYES,
  ONI_MARK_SIDES,
  ONI_MARK_VIEW_BOX,
} from "@onicode/shared/oniMark";
import { useId } from "react";
import type { ColorValue } from "react-native";
import Svg, { Mask, Path, Rect } from "react-native-svg";
import { withUniwind } from "uniwind";

const ThemedPath = withUniwind(Path);
const ASPECT_RATIO = 566 / 500;

/** The mochi oni, front-on, in one ink; its eyes are holes, so the background shows through. */
export function OniWordmark(props: {
  readonly height: number;
  readonly color?: ColorValue;
  readonly colorClassName?: string;
  /** Outline in mark units (the mark is 500 tall) that thickens the ink. */
  readonly strokeWidth?: number;
}) {
  const maskId = `oni-eyes-${useId().replace(/[^\w-]/g, "")}`;
  const ink = {
    color: props.color,
    colorClassName: props.colorClassName,
    fill: "currentColor",
    ...(props.strokeWidth
      ? { stroke: "currentColor", strokeWidth: props.strokeWidth, strokeLinejoin: "round" as const }
      : {}),
  };
  return (
    <Svg
      accessibilityLabel="Onicode"
      height={props.height}
      width={props.height * ASPECT_RATIO}
      viewBox={ONI_MARK_VIEW_BOX}
    >
      <Mask id={maskId} maskUnits="userSpaceOnUse" x="0" y="0" width="800" height="800">
        <Rect width="800" height="800" fill="#fff" />
        {ONI_MARK_SIDES.map((side) => (
          <Path
            key={side}
            d={ONI_MARK_EYES[side]}
            fill="none"
            stroke="#000"
            strokeWidth={ONI_MARK_EYE_STROKE_WIDTH}
            strokeLinecap="round"
          />
        ))}
      </Mask>
      {ONI_MARK_SIDES.map((side) => (
        <ThemedPath key={side} d={ONI_MARK_EARS[side]} {...ink} />
      ))}
      <ThemedPath d={ONI_MARK_BODY} mask={`url(#${maskId})`} {...ink} />
    </Svg>
  );
}
