import {
  deriveLatestContextWindowSnapshot,
  latestProviderTurnTokenUsage,
} from "@onicode/client-runtime/state/context-window";
import type { EnvironmentId, ThreadId } from "@onicode/contracts";
import { formatTokens } from "@onicode/shared/usageFormat";
import { Pressable, View } from "react-native";
import Svg, { Circle } from "react-native-svg";
import { useResolveClassNames } from "uniwind";

import { useThreadProjection } from "../../state/use-thread-detail";

const RADIUS = 6;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;
const NO_TURN_ITEMS: Parameters<typeof deriveLatestContextWindowSnapshot>[0] = [];

/** How full the thread's context window is, beside send; it opens `/context` when the provider has it. */
export function ComposerContextRing(props: {
  readonly environmentId: EnvironmentId;
  readonly threadId: ThreadId;
  readonly onOpen?: (() => void) | undefined;
}) {
  const projection =
    useThreadProjection({ environmentId: props.environmentId, threadId: props.threadId })
      ?.projection ?? null;
  const providerThread = projection?.providerThreads.find(
    (thread) => thread.id === projection.thread.activeProviderThreadId,
  );
  // Compaction rows only ever report used tokens without a window size, which this ring hides.
  const usage =
    projection === null
      ? null
      : deriveLatestContextWindowSnapshot(
          NO_TURN_ITEMS,
          latestProviderTurnTokenUsage(projection.providerTurns, projection, providerThread),
          providerThread,
        );
  const maxTokens = usage?.maxTokens != null && usage.maxTokens > 0 ? usage.maxTokens : null;
  const used =
    usage?.usedPercentage == null
      ? null
      : Math.round(Math.max(0, Math.min(100, usage.usedPercentage)));
  const track = useResolveClassNames("text-foreground/12").color;
  const tone = useResolveClassNames(
    used !== null && used >= 90
      ? "text-danger-foreground"
      : used !== null && used >= 80
        ? "text-warning-foreground"
        : "text-foreground-muted",
  ).color;
  if (usage === null || maxTokens === null || used === null) return null;
  const label = `${used}% of context used · ${formatTokens(usage.usedTokens)} / ${formatTokens(maxTokens)} tokens`;
  const ring = (
    <Svg width={18} height={18} viewBox="0 0 16 16" accessible={false}>
      <Circle cx={8} cy={8} r={RADIUS} fill="none" strokeWidth={2} stroke={track} />
      <Circle
        cx={8}
        cy={8}
        r={RADIUS}
        fill="none"
        strokeWidth={2}
        strokeLinecap="round"
        stroke={tone}
        strokeDasharray={`${(used / 100) * CIRCUMFERENCE} ${CIRCUMFERENCE}`}
        transform="rotate(-90 8 8)"
      />
    </Svg>
  );
  return props.onOpen ? (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={label}
      accessibilityHint="Shows what fills the context window"
      hitSlop={6}
      className="size-8 shrink-0 items-center justify-center active:opacity-70"
      onPress={props.onOpen}
    >
      {ring}
    </Pressable>
  ) : (
    <View
      accessible
      accessibilityRole="image"
      accessibilityLabel={label}
      className="size-8 shrink-0 items-center justify-center"
    >
      {ring}
    </View>
  );
}
