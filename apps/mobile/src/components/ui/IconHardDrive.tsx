import type { Icon } from "@tabler/icons-react-native";
import { forwardRef } from "react";
import Svg, { Path } from "react-native-svg";

/** Tabler has no drive glyph. Drawn on its 24px grid with its stroke, so it sits with the set. */
export const IconHardDrive: Icon = forwardRef(function IconHardDrive(
  { color = "currentColor", size = 24, strokeWidth = 2, title, ...rest },
  // React warns when a forwardRef render function takes one parameter.
  _ref,
) {
  const paint = {
    fill: "none",
    stroke: color,
    strokeWidth,
    strokeLinecap: "round",
    strokeLinejoin: "round",
  } as const;
  return (
    <Svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      {...(title ? { accessible: true, accessibilityLabel: title } : {})}
      {...rest}
    >
      <Path {...paint} d="M22 12H2" />
      <Path
        {...paint}
        d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"
      />
      <Path {...paint} d="M6 16h.01" />
      <Path {...paint} d="M10 16h.01" />
    </Svg>
  );
});
