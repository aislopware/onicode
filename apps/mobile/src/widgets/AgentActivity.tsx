import { HStack, Image, Spacer, Text, VStack } from "@expo/ui/swift-ui";
import type { ComponentProps } from "react";
import {
  accessibilityElement,
  accessibilityLabel,
  font,
  foregroundStyle,
  frame,
  layoutPriority,
  lineLimit,
  minimumScaleFactor,
  monospacedDigit,
  padding,
  resizable,
  widgetURL,
} from "@expo/ui/swift-ui/modifiers";
import type {
  PushActivityPhase,
  PushActivityRow,
  PushActivityState,
} from "@onicode/contracts/pushGateway";
import {
  createLiveActivity,
  type LiveActivityComponent,
  type LiveActivityLayout,
} from "expo-widgets";

type LiveActivityEnvironment = Parameters<LiveActivityComponent<AgentActivityProps>>[1];

export type AgentActivityPhase = PushActivityPhase;
export type AgentActivityRowProps = PushActivityRow;

/** What the gateway's `content-state` carries: one environment's agents. */
export type AgentActivityProps = PushActivityState & { readonly environmentId: string };

// Widget serialization excludes module helpers; use only imported view/modifier factories.
function AgentActivity(
  props: AgentActivityProps,
  environment: LiveActivityEnvironment,
): LiveActivityLayout {
  "widget";

  // Hierarchical foregrounds inherit system tint and vibrancy.
  type Foreground = Parameters<typeof foregroundStyle>[0];
  const primaryForeground = { type: "hierarchical", style: "primary" } as const;
  const secondaryForeground = { type: "hierarchical", style: "secondary" } as const;
  const monochrome =
    environment.widgetRenderingMode === "accented" || environment.widgetRenderingMode === "vibrant";

  const needsUser = (phase: AgentActivityPhase) =>
    phase === "waiting_for_approval" || phase === "waiting_for_input";
  const isWorking = (phase: AgentActivityPhase) => phase === "running" || phase === "starting";

  // The app's own status colours, in the variant for the surface: black on iPhone, white on a Mac.
  const isLightScheme = environment.colorScheme === "light";
  const phaseTint = (phase: AgentActivityPhase | undefined): Foreground => {
    if (environment.isLuminanceReduced) return secondaryForeground;
    if (monochrome) return primaryForeground;
    switch (phase) {
      case "waiting_for_approval":
        return isLightScheme ? "#d97706" : "#fcd34d";
      case "waiting_for_input":
        return isLightScheme ? "#4f46e5" : "#a5b4fc";
      case "failed":
        return isLightScheme ? "#dc2626" : "#fca5a5";
      case "completed":
        return isLightScheme ? "#059669" : "#6ee7b7";
      case "stale":
        return secondaryForeground;
      case "starting":
      case "running":
      default:
        return isLightScheme ? "#0284c7" : "#7dd3fc";
    }
  };

  const phasePriority = (phase: AgentActivityPhase): number => {
    if (needsUser(phase)) return 0;
    if (phase === "failed") return 1;
    if (phase === "completed") return 2;
    if (phase === "stale") return 4;
    return 3;
  };
  // Mark in-flight rows stale after expiry so they cannot claim agents still work.
  const activities: ReadonlyArray<AgentActivityRowProps> = environment.isStale
    ? props.activities.map((row) =>
        row.phase === "completed" || row.phase === "failed"
          ? row
          : { ...row, phase: "stale", status: "Out of date" },
      )
    : props.activities;
  // An environment older than this order still sends working rows ahead of finished ones.
  const ordered = [...activities].sort((a, b) => phasePriority(a.phase) - phasePriority(b.phase));
  const hero = ordered[0];
  const row1 = ordered[1];
  const row2 = ordered[2];
  const row3 = ordered[3];
  const accent = phaseTint(hero?.phase);

  const attentionCount = activities.filter((row) => needsUser(row.phase)).length;
  const failedCount = activities.filter((row) => row.phase === "failed").length;
  const doneCount = activities.filter((row) => row.phase === "completed").length;
  // Rows are capped, so the environment's count is the only one that covers every agent.
  const workingCount = Math.max(0, props.activeCount - attentionCount);
  const threadCount = props.activeCount + failedCount + doneCount;

  const workingLabel =
    workingCount === 0
      ? null
      : environment.isStale
        ? `${workingCount} out of date`
        : `${workingCount} working`;
  const pressing = [
    attentionCount > 0 ? `${attentionCount} need${attentionCount === 1 ? "s" : ""} you` : null,
    failedCount > 0 ? `${failedCount} failed` : null,
    doneCount > 0 ? `${doneCount} done` : null,
  ].filter((entry): entry is string => entry !== null);
  const alone =
    hero === undefined
      ? ""
      : hero.phase === "waiting_for_approval"
        ? "Needs approval"
        : hero.phase === "waiting_for_input"
          ? "Needs your input"
          : hero.phase === "failed"
            ? "Failed"
            : hero.phase === "completed"
              ? "Done"
              : hero.phase === "stale"
                ? "Out of date"
                : "Working";
  // The rows spell out the rest, so the card names only the most pressing count and the work behind it.
  const headline = threadCount > 1 ? (pressing[0] ?? workingLabel ?? "") : alone;
  const headlineTint: Foreground =
    hero === undefined || isWorking(hero.phase) ? primaryForeground : accent;
  const remainder =
    threadCount > 1 && pressing.length > 0 && workingLabel !== null ? workingLabel : "";
  const spoken =
    [...pressing, workingLabel ?? ""].filter(Boolean).join(", ") || alone || "No agent activity";
  // How many agents share the state the island's glyph shows.
  const heroCount =
    hero === undefined
      ? 0
      : needsUser(hero.phase)
        ? attentionCount
        : hero.phase === "failed"
          ? failedCount
          : hero.phase === "completed"
            ? doneCount
            : workingCount;

  // Widget taps target the containing app, so its production scheme is safe in every build.
  const deepLink =
    hero && hero.deepLink.startsWith("/") && !hero.deepLink.startsWith("//")
      ? `onicode://${hero.deepLink.slice(1)}`
      : null;
  const linkModifiers = deepLink ? [widgetURL(deepLink)] : [];

  type SFName = NonNullable<ComponentProps<typeof Image>["systemName"]>;
  // One family of circles, so the glyph column reads as a single shape in different states.
  const phaseSymbol = (phase: AgentActivityPhase): SFName => {
    switch (phase) {
      case "waiting_for_approval":
        return "exclamationmark.circle.fill";
      case "waiting_for_input":
        return "questionmark.circle.fill";
      case "failed":
        return "xmark.circle.fill";
      case "completed":
        return "checkmark.circle.fill";
      case "starting":
        return "circle.dotted";
      case "stale":
        return "clock.arrow.circlepath";
      case "running":
      default:
        // Matches the thread list's Working indicator.
        return "circle.dashed";
    }
  };

  // SF Symbols, like the logo, ignore frame/foregroundStyle applied directly to
  // the image; size + tint them through a container the resizable symbol fills.
  const renderGlyph = (
    systemName: SFName,
    size: number,
    color: Foreground,
    modifiers: typeof linkModifiers = [],
  ) => (
    <HStack
      modifiers={[frame({ width: size, height: size }), foregroundStyle(color), ...modifiers]}
    >
      <Image systemName={systemName} modifiers={[resizable()]} />
    </HStack>
  );

  // Image views only honor `resizable` directly (frame/foregroundStyle are
  // dropped), so we size it via a container frame the resizable image fills and
  // tint it through the container's foreground style, which the template image
  // inherits.
  const renderLogo = (height: number, color: Foreground, modifiers: typeof linkModifiers = []) => (
    <HStack
      modifiers={[frame({ width: height * 1.5, height }), foregroundStyle(color), ...modifiers]}
    >
      <Image assetName="OniMark" modifiers={[resizable()]} />
    </HStack>
  );

  // The mark and every row's glyph sit in one column, so all text shares a left edge.
  const MARK_COLUMN = 20;
  const renderMarkColumn = (mark: ReturnType<typeof renderGlyph>) => (
    <HStack spacing={0} modifiers={[frame({ width: MARK_COLUMN })]}>
      {mark}
    </HStack>
  );

  // The island's side regions fit the headline alone; the card has room for the work behind it.
  const renderSummary = (withRemainder: boolean) => (
    <HStack spacing={4} alignment="center" modifiers={[layoutPriority(1)]}>
      <Text
        modifiers={[
          font({ weight: "semibold", size: 13 }),
          foregroundStyle(headlineTint),
          monospacedDigit(),
          lineLimit(1),
          minimumScaleFactor(0.85),
        ]}
      >
        {headline}
      </Text>
      {withRemainder && remainder ? (
        <Text
          modifiers={[
            font({ size: 13 }),
            foregroundStyle(secondaryForeground),
            monospacedDigit(),
            lineLimit(1),
          ]}
        >
          {`· ${remainder}`}
        </Text>
      ) : null}
    </HStack>
  );

  const renderEnvironment = () => (
    <Text modifiers={[font({ size: 13 }), foregroundStyle(secondaryForeground), lineLimit(1)]}>
      {props.title}
    </Text>
  );

  // The project keeps its full width so the column stays even, which a long name must not turn
  // into a title with no room.
  const shortProject = (title: string) => {
    const letters = Array.from(title);
    return letters.length > 14 ? `${letters.slice(0, 13).join("")}…` : title;
  };

  // Every thread gets the same line. Its state is the glyph; the words for it are in the summary.
  const renderRow = (row: AgentActivityRowProps) => (
    <HStack
      spacing={8}
      alignment="center"
      modifiers={[
        accessibilityElement("ignore"),
        accessibilityLabel(`${row.threadTitle}, ${row.status}, ${row.projectTitle}`),
      ]}
    >
      {renderMarkColumn(renderGlyph(phaseSymbol(row.phase), 16, phaseTint(row.phase)))}
      <Text
        modifiers={[
          font({ weight: "medium", size: 15 }),
          foregroundStyle(primaryForeground),
          lineLimit(1),
        ]}
      >
        {row.threadTitle}
      </Text>
      <Spacer minLength={0} />
      <Text
        modifiers={[
          font({ size: 13 }),
          foregroundStyle(secondaryForeground),
          lineLimit(1),
          layoutPriority(1),
        ]}
      >
        {shortProject(row.projectTitle)}
      </Text>
    </HStack>
  );

  return {
    banner: (
      <VStack alignment="leading" spacing={10} modifiers={[padding({ all: 14 }), ...linkModifiers]}>
        <HStack spacing={8} alignment="center">
          {renderMarkColumn(renderLogo(12, secondaryForeground))}
          {renderEnvironment()}
          <Spacer minLength={10} />
          {renderSummary(true)}
        </HStack>
        {hero ? (
          <VStack alignment="leading" spacing={8}>
            {renderRow(hero)}
            {row1 ? renderRow(row1) : null}
            {row2 ? renderRow(row2) : null}
            {row3 ? renderRow(row3) : null}
          </VStack>
        ) : null}
      </VStack>
    ),
    bannerSmall: (
      <VStack alignment="leading" spacing={4} modifiers={[padding({ all: 10 })]}>
        <HStack spacing={6} alignment="center">
          {hero
            ? renderGlyph(phaseSymbol(hero.phase), 14, accent)
            : renderLogo(12, secondaryForeground)}
          {renderSummary(false)}
          <Spacer minLength={6} />
        </HStack>
        {hero ? (
          <Text
            modifiers={[
              font({ weight: "medium", size: 13 }),
              foregroundStyle(primaryForeground),
              lineLimit(1),
            ]}
          >
            {hero.threadTitle}
          </Text>
        ) : null}
      </VStack>
    ),
    compactLeading: renderLogo(14, accent, linkModifiers),
    // The glyph names the most pressing state, and a number joins it only when it covers several.
    compactTrailing: (
      <HStack
        spacing={3}
        alignment="center"
        modifiers={[
          padding({ trailing: 2 }),
          accessibilityElement("ignore"),
          accessibilityLabel(spoken),
          ...linkModifiers,
        ]}
      >
        {heroCount > 1 ? (
          <Text
            modifiers={[
              font({ weight: "semibold", size: 13 }),
              foregroundStyle(accent),
              monospacedDigit(),
              lineLimit(1),
              minimumScaleFactor(0.7),
            ]}
          >
            {heroCount > 99 ? "99+" : `${heroCount}`}
          </Text>
        ) : null}
        {renderGlyph(phaseSymbol(hero?.phase ?? "running"), 15, accent)}
      </HStack>
    ),
    minimal: hero
      ? renderGlyph(phaseSymbol(hero.phase), 15, accent, linkModifiers)
      : renderLogo(11, accent, linkModifiers),
    // The mark stays where the compact view has it, and the state stays on the trailing side.
    expandedLeading: (
      <HStack spacing={8} alignment="center" modifiers={[padding({ leading: 6, vertical: 4 })]}>
        {renderMarkColumn(renderLogo(12, accent))}
        {renderEnvironment()}
      </HStack>
    ),
    expandedCenter: null,
    expandedTrailing: (
      <HStack spacing={0} modifiers={[padding({ trailing: 6, vertical: 4 })]}>
        {renderSummary(false)}
      </HStack>
    ),
    expandedBottom: (
      <VStack
        alignment="leading"
        spacing={8}
        modifiers={[padding({ top: 4, bottom: 6, horizontal: 6 }), ...linkModifiers]}
      >
        {hero ? renderRow(hero) : null}
        {row1 ? renderRow(row1) : null}
        {row2 ? renderRow(row2) : null}
      </VStack>
    ),
  };
}

export default createLiveActivity<AgentActivityProps>("AgentActivity", AgentActivity);
