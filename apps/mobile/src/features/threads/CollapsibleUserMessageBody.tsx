import { T } from "gt-react-native";
import { useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import {
  Pressable,
  View,
  type ColorValue,
  type LayoutChangeEvent,
  type ViewInstance,
} from "react-native";
import Svg, { Defs, LinearGradient, Rect, Stop } from "react-native-svg";
import { AppText as Text } from "../../components/ui/AppText";

const FOLDED_HEIGHT = 176;

// Fold only when opening at least doubles what is on screen, matching web's useTallEnoughToFold.
const isTallEnoughToFold = (height: number) => height >= FOLDED_HEIGHT * 2;

export function CollapsibleUserMessageBody(props: {
  readonly collapsible: boolean;
  readonly expanded: boolean;
  readonly onToggle: () => void;
  readonly bubbleColor: ColorValue;
  readonly children: ReactNode;
}) {
  const gradientId = `user-message-fade-${useId().replaceAll(":", "")}`;
  const contentRef = useRef<ViewInstance>(null);
  const measuredWidth = useRef<number | null>(null);
  const [tallEnough, setTallEnough] = useState(false);
  const [remeasure, setRemeasure] = useState(0);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- the fold depends on the rendered height; measuring before paint avoids one unfolded frame
  useLayoutEffect(() => {
    const rect = contentRef.current?.getBoundingClientRect();
    if (!rect) return;
    measuredWidth.current = rect.width;
    setTallEnough(isTallEnoughToFold(rect.height));
    // oxlint-disable-next-line react/exhaustive-effect-dependencies -- a width change while folded measures the unfolded text again.
  }, [remeasure]);
  const canCollapse = props.collapsible && tallEnough;
  const collapsed = canCollapse && !props.expanded;

  return (
    <View>
      <View style={collapsed ? { maxHeight: FOLDED_HEIGHT, overflow: "hidden" } : undefined}>
        <View
          ref={contentRef}
          onLayout={(event: LayoutChangeEvent) => {
            const { width, height } = event.nativeEvent.layout;
            if (!collapsed) {
              measuredWidth.current = width;
              setTallEnough(isTallEnoughToFold(height));
              return;
            }
            // Folded, Yoga reports the clipped height, which would read as short and unfold it in a loop.
            // A new width can make the text fit, so unfold once and measure again before paint.
            if (Math.abs(width - (measuredWidth.current ?? width)) >= 1) {
              measuredWidth.current = width;
              setTallEnough(false);
              setRemeasure((count) => count + 1);
            }
          }}
        >
          {props.children}
        </View>
        {collapsed ? (
          <View pointerEvents="none" className="absolute inset-x-0 bottom-0 h-7">
            <Svg width="100%" height="100%">
              <Defs>
                <LinearGradient id={gradientId} x1="0%" x2="0%" y1="0%" y2="100%">
                  <Stop offset={0} stopColor={props.bubbleColor} stopOpacity={0} />
                  <Stop offset={1} stopColor={props.bubbleColor} stopOpacity={1} />
                </LinearGradient>
              </Defs>
              <Rect width="100%" height="100%" fill={`url(#${gradientId})`} />
            </Svg>
          </View>
        ) : null}
      </View>
      {canCollapse ? (
        <Pressable
          accessibilityRole="button"
          accessibilityState={{ expanded: props.expanded }}
          onPress={props.onToggle}
          className="mt-1.5 min-h-11 items-end justify-center active:opacity-70"
        >
          <Text className="font-oni-medium text-xs text-user-bubble-foreground-muted">
            {props.expanded ? <T>Show less</T> : <T>Show full message</T>}
          </Text>
        </Pressable>
      ) : null}
    </View>
  );
}
