import { useLocale, useMessages } from "gt-react";
import { useState, useSyncExternalStore } from "react";

import { getI18nSnapshot, subscribeI18n } from "../i18n/runtime";
import {
  deriveTimelineEntriesFromVisibleTurnItemsWithState,
  type TimelineEntriesInput,
  type TimelineEntriesProjection,
} from "../session-logic";

function sameInput(left: TimelineEntriesInput, right: TimelineEntriesInput): boolean {
  return (
    left.visibleTurnItems === right.visibleTurnItems &&
    left.optimisticMessages === right.optimisticMessages &&
    left.anchoredMessages === right.anchoredMessages &&
    left.attachmentUrlById === right.attachmentUrlById &&
    left.attempts === right.attempts &&
    left.nodes === right.nodes &&
    left.plans === right.plans
  );
}

export function useIncrementalTimelineEntries(
  threadKey: string | null,
  timelineInput: TimelineEntriesInput,
): TimelineEntriesProjection["entries"] {
  // Keyed on the locale and catalog: without a GTProvider, useMessages() returns a new function
  // every render.
  const locale = useLocale();
  const catalog = useSyncExternalStore(subscribeI18n, getI18nSnapshot);
  const resolve = useMessages();
  const input = { ...timelineInput, resolve: timelineInput.resolve ?? resolve };
  const [state, setState] = useState(() => ({
    threadKey,
    locale,
    catalog,
    projection: deriveTimelineEntriesFromVisibleTurnItemsWithState(input),
  }));
  const reusable =
    state.threadKey === threadKey && state.locale === locale && state.catalog === catalog;
  if (reusable && sameInput(state.projection.input, input)) {
    return state.projection.entries;
  }
  const projection = deriveTimelineEntriesFromVisibleTurnItemsWithState(
    input,
    reusable ? state.projection : null,
  );
  setState({ threadKey, locale, catalog, projection });
  return projection.entries;
}
