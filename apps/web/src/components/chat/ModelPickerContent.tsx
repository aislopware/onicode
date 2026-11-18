import { T, useGT, useLocale, useMessages } from "gt-react";
import { useLegendListRef } from "~/hooks/useLegendListRef";
import { Combobox as ComboboxPrimitive } from "@base-ui/react/combobox";
import {
  ANTIGRAVITY_DEFAULT_MODEL,
  type ProviderInstanceId,
  type EnvironmentId,
  type ProviderDriverKind,
  type ResolvedKeybindingsConfig,
  MODEL_PICKER_JUMP_KEYBINDING_COMMANDS,
} from "@onicode/contracts";
import { getProviderOptionCurrentLabel, resolveSelectableModel } from "@onicode/shared/model";
import { useAtomValue } from "@effect/atom-react";
import { formatProviderUpdateRequiredNotice } from "@onicode/client-runtime/providerUpdateRequiredModels";
import { useNavigate } from "@tanstack/react-router";
import { LegendList, type LegendListRef } from "@legendapp/list/react";
import {
  memo,
  useState,
  useEffect,
  useEffectEvent,
  useLayoutEffect,
  useRef,
  useSyncExternalStore,
  type PointerEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { ModelListRow, ModelPickerRowItem } from "./ModelListRow";
import { ModelDetailCard, type ModelDetail } from "./ModelDetailCard";
import { useProviderWorkspaceSnapshotRefresh } from "./useProviderWorkspaceSnapshotRefresh";
import { getProviderStatusMessage, hasProviderSetup } from "./ProviderStatusBanner";
import { ProviderInstanceIcon } from "./ProviderInstanceIcon";
import {
  modelPickerFavoriteKey,
  modelPickerLegacySectionKey,
  modelPickerModelKey,
  parseModelPickerLegacySectionKey,
  parseModelPickerModelKey,
} from "./modelPickerKeys";
import { rankModelPickerSearch } from "@onicode/client-runtime/model-search";
import { Combobox, ComboboxEmpty, ComboboxListVirtualized } from "../ui/combobox";
import { ModelEsque } from "./providerIconUtils";
import { isCommandPaletteOpen } from "../../commandPaletteBus";
import { registerModelPickerContent } from "../../modelPickerVisibility";
import { primaryServerKeybindingsAtom } from "../../state/server";
import {
  modelPickerJumpCommandForIndex,
  modelPickerJumpIndexFromCommand,
  resolveShortcutCommand,
  shortcutLabelForCommand,
} from "../../keybindings";
import {
  useEnvironmentModelPreferences,
  useToggleEnvironmentModelFavorite,
} from "~/hooks/useSettings";
import { announce } from "~/lib/announce";
import { observeResize } from "~/lib/observeResize";
import { cn } from "~/lib/utils";
import { getVirtualizedScrollFadeClassName } from "../ui/scroll-area";
import { Tooltip, TooltipPopup, TooltipProvider, TooltipTrigger } from "../ui/tooltip";
import { InlineButton } from "../ui/button";
import {
  isProviderInstancePickerReady,
  isProviderInstancePickerVisible,
  shouldShowInstanceBadge,
  type ProviderInstanceEntry,
} from "../../providerInstances";
import { providerModelKey, sortProviderModelItems } from "../../modelOrdering";
import { hasRuntimeModelCatalog } from "../../providerModels";

type ModelPickerItem = {
  slug: string;
  name: string;
  shortName?: string;
  subProvider?: string;
  badge?: "new";
  instanceId: ProviderInstanceId;
  driverKind: ProviderDriverKind;
  instanceDisplayName: string;
  instanceAccentColor?: string | undefined;
  acpRegistryAgentId?: string | undefined;
  acpRegistryIconUrl?: string | undefined;
  continuationGroupKey?: string | undefined;
  isLegacy?: boolean | undefined;
  isUnavailable?: boolean | undefined;
};

export function resolveModelPickerSelectedModel(input: {
  driverKind: ProviderDriverKind | undefined;
  model: string;
  options: ReadonlyArray<ModelEsque>;
}) {
  if (input.driverKind === "antigravity" && input.model === ANTIGRAVITY_DEFAULT_MODEL) {
    const availableModels = input.options.filter(
      (option) => option.slug !== ANTIGRAVITY_DEFAULT_MODEL && !option.isUnavailable,
    );
    return (
      availableModels.find((option) => option.aliases?.includes(ANTIGRAVITY_DEFAULT_MODEL)) ??
      availableModels.find((option) => option.isDefault)
    );
  }
  return input.options.find((option) => option.slug === input.model);
}

function shouldIncludeModelPickerOption(input: {
  readonly entry: ProviderInstanceEntry;
  readonly option: ModelEsque;
  readonly activeInstanceId: ProviderInstanceId;
  readonly activeModel: string;
}): boolean {
  if (input.entry.driverKind === "antigravity" && input.option.slug === ANTIGRAVITY_DEFAULT_MODEL) {
    return false;
  }
  if (isProviderInstancePickerReady(input.entry)) return true;
  return (
    input.entry.enabled &&
    hasRuntimeModelCatalog(input.entry.driverKind) &&
    input.entry.instanceId === input.activeInstanceId &&
    input.option.slug === input.activeModel &&
    input.option.isUnavailable === true
  );
}

function shouldOfferModelPickerSetup(
  entry: ProviderInstanceEntry,
  options: ReadonlyArray<ModelEsque>,
): boolean {
  return (
    entry.enabled &&
    entry.status !== "disabled" &&
    hasProviderSetup(entry.snapshot) &&
    (!isProviderInstancePickerReady(entry) ||
      !entry.installed ||
      entry.snapshot.auth.status === "unauthenticated" ||
      !options.some((option) => !option.isUnavailable))
  );
}

const EMPTY_MODEL_JUMP_LABELS = new Map<string, string>();
const ROW_PX = 28;
const NOTICE_ESTIMATED_PX = 56;
const LIST_MAX_PX = 320;
const LIST_PADDING_PX = 8;
const CARD_GAP_PX = 11;
const CARD_VIEWPORT_MARGIN_PX = 8;
const FAVORITES_SECTION_KEY = "section:favorites";

type ListRow =
  | {
      kind: "header";
      key: string;
      label: string;
      entry: ProviderInstanceEntry | null;
      /** A provider this thread cannot switch to. */
      lockedOut: boolean;
    }
  | { kind: "model"; key: string; model: ModelPickerItem }
  | { kind: "legacy"; key: string; expanded: boolean }
  | { kind: "setup"; key: string; entry: ProviderInstanceEntry; label: string }
  | { kind: "notice"; key: string; notice: string };

function isHighlightableRow(row: ListRow): boolean {
  return row.kind === "model" || row.kind === "legacy";
}

function estimatedRowHeight(row: ListRow): number {
  return row.kind === "setup" || row.kind === "notice" ? NOTICE_ESTIMATED_PX : ROW_PX;
}

/** The context window a model runs with unless the thread picks another. */
function modelContextLabel(entry: ProviderInstanceEntry | undefined, slug: string): string | null {
  const descriptor = entry?.models
    .find((model) => model.slug === slug)
    ?.capabilities?.optionDescriptors?.find((candidate) => candidate.id === "contextWindow");
  return getProviderOptionCurrentLabel(descriptor) ?? null;
}

function modelJumpLabelsByKey(
  modelKeys: ReadonlyArray<string>,
  keybindings: ResolvedKeybindingsConfig,
  terminalOpen: boolean,
): ReadonlyMap<string, string> {
  const shortcutLabelOptions = {
    platform: navigator.platform,
    context: { terminalFocus: false, terminalOpen, modelPickerOpen: true },
  };
  const mapping = new Map<string, string>();
  for (const [index, modelKey] of modelKeys.entries()) {
    const command = modelPickerJumpCommandForIndex(index);
    const label = command && shortcutLabelForCommand(keybindings, command, shortcutLabelOptions);
    if (label) {
      mapping.set(modelKey, label);
    }
  }
  return mapping.size > 0 ? mapping : EMPTY_MODEL_JUMP_LABELS;
}

function SectionHeader(props: { row: Extract<ListRow, { kind: "header" }> }) {
  const gt = useGT();
  const { entry, lockedOut } = props.row;
  const header = (
    <div
      className={cn(
        "flex h-7 items-end gap-1.5 bg-popover px-2 pb-1 text-xs font-medium text-muted-foreground",
        lockedOut && "opacity-50",
      )}
    >
      {entry ? (
        <ProviderInstanceIcon
          driverKind={entry.driverKind}
          displayName={entry.displayName}
          accentColor={entry.accentColor}
          acpRegistryAgentId={entry.acpRegistryAgentId}
          acpRegistryIconUrl={entry.acpRegistryIconUrl}
          className="mb-px size-3"
          iconClassName="size-3"
        />
      ) : null}
      <span className="min-w-0 truncate">{props.row.label}</span>
    </div>
  );
  if (!lockedOut) return header;
  return (
    <Tooltip>
      <TooltipTrigger render={header} />
      <TooltipPopup side="left" align="center">
        {gt("{provider} is unavailable in this thread. Start a new thread to switch providers.", {
          provider: props.row.label,
        })}
      </TooltipPopup>
    </Tooltip>
  );
}

function ProviderSetupRow(props: {
  entry: ProviderInstanceEntry;
  label: string;
  onRequestClose: (() => void) | undefined;
  onOpenProviderSetup: ((instanceId: ProviderInstanceId) => void) | undefined;
}) {
  const messages = useMessages();
  return (
    <div className="px-2 py-1.5 text-xs leading-snug">
      <p className="line-clamp-3 text-muted-foreground">
        {getProviderStatusMessage(props.entry.snapshot, messages)}
      </p>
      <InlineButton
        className="mt-1"
        onClick={() => {
          props.onRequestClose?.();
          props.onOpenProviderSetup?.(props.entry.instanceId);
        }}
      >
        {props.label}
      </InlineButton>
    </div>
  );
}

function UpdateRequiredNotice(props: {
  notice: string;
  onRequestClose: (() => void) | undefined;
  className?: string;
}) {
  const navigate = useNavigate();
  return (
    <p className={cn("px-2 py-1.5 text-xs leading-snug text-muted-foreground", props.className)}>
      {props.notice}{" "}
      <InlineButton
        onClick={() => {
          props.onRequestClose?.();
          void navigate({ to: "/settings/providers" });
        }}
      >
        <T>Provider settings</T>
      </InlineButton>
    </p>
  );
}

function ModelPickerList({
  listRef,
  rows,
  height,
  stickyHeaderIndices,
  initialScrollIndex,
  extraData,
  onScroll,
  onPointerMove,
  renderItem,
}: {
  listRef: RefObject<LegendListRef | null>;
  rows: ReadonlyArray<ListRow>;
  height: number | undefined;
  stickyHeaderIndices: number[] | undefined;
  initialScrollIndex: number | undefined;
  extraData: unknown;
  onScroll: () => void;
  onPointerMove: (event: PointerEvent<HTMLElement>) => void;
  renderItem: (props: { item: ListRow; index: number }) => ReactNode;
}) {
  const [showBottomScrollFade, setShowBottomScrollFade] = useState(false);
  const [showTopScrollFade, setShowTopScrollFade] = useState(false);
  const updateScrollFades = () => {
    const metrics = listRef.current?.getState();
    if (!metrics) return;
    // A sticky header already marks the top edge.
    setShowTopScrollFade(!metrics.isAtStart && stickyHeaderIndices === undefined);
    setShowBottomScrollFade(!metrics.isAtEnd);
  };
  const scrollRef = useLegendListRef(listRef, updateScrollFades);

  return (
    <div
      className={cn(
        "relative min-h-0 overflow-hidden px-1",
        rows.length === 0 ? "h-0" : height === undefined ? "flex-1" : "shrink",
      )}
      style={height === undefined ? undefined : { height }}
      onPointerMove={onPointerMove}
    >
      <ComboboxListVirtualized>
        <LegendList<ListRow>
          ref={scrollRef}
          data={rows}
          extraData={extraData}
          keyExtractor={(row) => row.key}
          renderItem={renderItem}
          getItemType={(row) => row.kind}
          getFixedItemSize={(row) => (estimatedRowHeight(row) === ROW_PX ? ROW_PX : undefined)}
          estimatedItemSize={ROW_PX}
          drawDistance={480}
          recycleItems
          {...(stickyHeaderIndices ? { stickyHeaderIndices } : {})}
          {...(initialScrollIndex !== undefined
            ? { initialScrollIndex: { index: initialScrollIndex, viewPosition: 0.5 } }
            : {})}
          onLayout={updateScrollFades}
          onScroll={() => {
            updateScrollFades();
            onScroll();
          }}
          onReady={updateScrollFades}
          className={cn(
            "h-full overflow-x-hidden overscroll-y-contain py-1 [scrollbar-width:thin]",
            getVirtualizedScrollFadeClassName({
              top: showTopScrollFade,
              bottom: showBottomScrollFade,
            }),
          )}
        />
      </ComboboxListVirtualized>
    </div>
  );
}

// Capture gives picker shortcuts priority over the composer.
function WindowKeyDownListener({ onKeyDown }: { onKeyDown: (event: KeyboardEvent) => void }) {
  const handleKeyDown = useEffectEvent(onKeyDown);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- subscribes a capture-phase window keydown listener for the picker's lifetime
  useEffect(() => {
    const listener = (event: KeyboardEvent) => handleKeyDown(event);
    window.addEventListener("keydown", listener, true);
    return () => {
      window.removeEventListener("keydown", listener, true);
    };
  }, []);
  return null;
}

/**
 * Which row the detail card describes, and a nudge to re-place it. A store, not state, so a
 * hover re-renders the card alone, never the list.
 */
function createCardStore() {
  let key: string | null = null;
  const listeners = new Set<() => void>();
  const placers = new Set<() => void>();
  return {
    get: () => key,
    show: (next: string) => {
      if (next === key) return;
      key = next;
      for (const listener of listeners) listener();
    },
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => void listeners.delete(listener);
    },
    place: () => {
      for (const placer of placers) placer();
    },
    onPlace: (placer: () => void) => {
      placers.add(placer);
      return () => void placers.delete(placer);
    },
  };
}
type CardStore = ReturnType<typeof createCardStore>;

type CardModel = ModelDetail & { key: string; instanceId: ProviderInstanceId; slug: string };

/**
 * Floats the detail card beside the list, its top on the row it describes. It stays inside the
 * window with a margin, and moves to the list's left when the right has no room.
 */
function FloatingModelCard(props: {
  store: CardStore;
  host: HTMLElement;
  fallbackKey: string | null;
  resolve: (key: string) => CardModel | null;
  activeModelKey: string | null;
  controls: ReactNode;
  onToggleFavorite: (instanceId: ProviderInstanceId, slug: string) => void;
}) {
  const { store, host } = props;
  const cardRef = useRef<HTMLDivElement>(null);
  const shownKey = useSyncExternalStore(store.subscribe, store.get) ?? props.fallbackKey;
  const model =
    (shownKey ? props.resolve(shownKey) : null) ??
    (props.fallbackKey ? props.resolve(props.fallbackKey) : null);

  const place = useEffectEvent(() => {
    const card = cardRef.current;
    if (!card || !model) return;
    const hostRect = host.getBoundingClientRect();
    // The popup scales in; positions are set in its unscaled pixels.
    const scale = host.offsetWidth > 0 ? hostRect.width / host.offsetWidth : 1;
    const row = host.querySelector<HTMLElement>(
      `[data-model-picker-row="${CSS.escape(model.key)}"]`,
    );
    const list = host.querySelector<HTMLElement>("[data-slot=combobox-list]");
    const rowRect = row?.getBoundingClientRect();
    const listRect = list?.getBoundingClientRect();
    const rowShown =
      rowRect && listRect && rowRect.bottom > listRect.top && rowRect.top < listRect.bottom;
    const anchor = rowShown ? rowRect.top : hostRect.top;
    const height = card.offsetHeight * scale;
    const top = Math.max(
      CARD_VIEWPORT_MARGIN_PX,
      Math.min(anchor, window.innerHeight - CARD_VIEWPORT_MARGIN_PX - height),
    );
    const width = card.offsetWidth * scale;
    const fitsRight =
      hostRect.right + CARD_GAP_PX + width <= window.innerWidth - CARD_VIEWPORT_MARGIN_PX;
    // Offsets start inside the popup's border.
    card.style.top = `${(top - hostRect.top) / scale - host.clientTop}px`;
    card.style.left = fitsRight
      ? `${host.offsetWidth - host.clientLeft + CARD_GAP_PX}px`
      : `${-host.clientLeft - CARD_GAP_PX - card.offsetWidth}px`;
  });

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- re-places the card when the list scrolls, the card resizes or the window does
  useEffect(() => {
    const stopPlace = store.onPlace(place);
    const card = cardRef.current;
    const stopResize = card ? observeResize(card, place) : () => {};
    window.addEventListener("resize", place);
    return () => {
      stopPlace();
      stopResize();
      window.removeEventListener("resize", place);
    };
  }, [store]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- the row the card follows moves with every render of the list, so it is measured after each one before paint
  useLayoutEffect(() => place());

  if (!model) return null;
  return createPortal(
    <ModelDetailCard
      ref={cardRef}
      floating
      model={model}
      controls={model.key === props.activeModelKey ? props.controls : undefined}
      onToggleFavorite={() => props.onToggleFavorite(model.instanceId, model.slug)}
    />,
    host,
  );
}

export const ModelPickerContent = memo(function ModelPickerContent(props: {
  environmentId: EnvironmentId | null;
  /** Keeps the selected provider's catalog fresh for this directory. */
  workspaceCwd?: string | null;
  activeInstanceId: ProviderInstanceId;
  model: string;
  selectedModels?: ReadonlyArray<{ instanceId: ProviderInstanceId; model: string }>;
  onToggleModel?: (instanceId: ProviderInstanceId, model: string) => void;
  lockedProvider: ProviderDriverKind | null;
  lockedContinuationGroupKey?: string | null;
  instanceEntries: ReadonlyArray<ProviderInstanceEntry>;
  keybindings?: ResolvedKeybindingsConfig;
  modelOptionsByInstance: ReadonlyMap<ProviderInstanceId, ReadonlyArray<ModelEsque>>;
  terminalOpen: boolean;
  onRequestClose?: () => void;
  onOpenProviderSetup?: (instanceId: ProviderInstanceId) => void;
  getModelDisabledReason?: (instanceId: ProviderInstanceId, model: string) => string | null;
  onInstanceModelChange: (instanceId: ProviderInstanceId, model: string) => void;
  /** The owning popup moves focus here when it opens (its `initialFocus`). */
  searchInputRef: RefObject<HTMLInputElement | null>;
  /** The selected model's effort section, shown on its detail card. */
  controls?: ReactNode;
  /** Right at the end of the search text moves into the effort section. */
  onFocusEffort?: () => void;
  /** Closes the list. */
  listFooter?: ReactNode;
  /**
   * The popup the detail card floats beside. Null puts the card's content under the list for
   * the selected model, for windows with no room beside it.
   */
  cardHost: HTMLElement | null;
}) {
  const gt = useGT();
  const messages = useMessages();
  const locale = useLocale();
  const {
    searchInputRef,
    keybindings: providedKeybindings,
    modelOptionsByInstance,
    instanceEntries,
    getModelDisabledReason,
    onInstanceModelChange,
    onToggleModel,
    onRequestClose,
    onOpenProviderSetup,
  } = props;
  const [searchQuery, setSearchQuery] = useState("");
  const modelListRef = useRef<LegendListRef | null>(null);
  const pickerContentRef = useRef<HTMLDivElement>(null);
  const [cardStore] = useState(createCardStore);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- publishes this picker's mount to global keydown handlers
  useLayoutEffect(() => registerModelPickerContent(), []);
  const highlightedModelKeyRef = useRef<string | null>(null);
  const sectionCursorRef = useRef(-1);
  const { favorites } = useEnvironmentModelPreferences(props.environmentId);
  const activeEntry = props.instanceEntries.find(
    (entry) => entry.instanceId === props.activeInstanceId,
  );
  const activeModel = resolveModelPickerSelectedModel({
    driverKind: activeEntry?.driverKind,
    model: props.model,
    options: modelOptionsByInstance.get(props.activeInstanceId) ?? [],
  });
  const activeModelSlug =
    activeModel?.slug ?? (props.model === ANTIGRAVITY_DEFAULT_MODEL ? "" : props.model);
  const activeModelKey = activeModelSlug
    ? modelPickerModelKey(props.activeInstanceId, activeModelSlug)
    : null;
  const selectedModelKeys = props.selectedModels?.map((selection) => {
    const entry = instanceEntries.find((entry) => entry.instanceId === selection.instanceId);
    const model = resolveModelPickerSelectedModel({
      driverKind: entry?.driverKind,
      model: selection.model,
      options: modelOptionsByInstance.get(selection.instanceId) ?? [],
    });
    return modelPickerModelKey(selection.instanceId, model?.slug ?? selection.model);
  });
  const selectedModelKeySet = new Set(
    selectedModelKeys ?? (activeModelKey ? [activeModelKey] : []),
  );
  const { scanning: workspaceModelsScanning } = useProviderWorkspaceSnapshotRefresh({
    environmentId: props.environmentId,
    gitCwd: props.workspaceCwd ?? null,
    instanceId: props.activeInstanceId,
  });
  const favoritesSet = new Set(favorites.map((fav) => providerModelKey(fav.provider, fav.model)));
  const isFavorite = (model: { instanceId: ProviderInstanceId; slug: string }) =>
    favoritesSet.has(providerModelKey(model.instanceId, model.slug));
  const [expandedLegacyInstances, setExpandedLegacyInstances] = useState(
    () =>
      new Set<ProviderInstanceId>(
        activeModel?.isLegacy &&
          !favoritesSet.has(providerModelKey(props.activeInstanceId, activeModelSlug))
          ? [props.activeInstanceId]
          : [],
      ),
  );
  const serverKeybindings = useAtomValue(primaryServerKeybindingsAtom);
  const keybindings = providedKeybindings ?? serverKeybindings;
  const [searchHeight, setSearchHeight] = useState(0);
  const toggleFavorite = useToggleEnvironmentModelFavorite(props.environmentId);

  const toggleLegacySection = (instanceId: ProviderInstanceId) => {
    setExpandedLegacyInstances((expanded) => {
      const next = new Set(expanded);
      if (next.has(instanceId)) {
        next.delete(instanceId);
      } else {
        next.add(instanceId);
      }
      return next;
    });
  };

  const handleModelSelect = (
    modelSlug: string,
    instanceId: ProviderInstanceId,
    additive = false,
  ): string | null => {
    if (getModelDisabledReason?.(instanceId, modelSlug)) {
      return null;
    }
    const options = modelOptionsByInstance.get(instanceId);
    if (!options) {
      return null;
    }
    const entry = instanceEntries.find((candidate) => candidate.instanceId === instanceId);
    if (!entry) {
      return null;
    }
    const resolvedModel = resolveSelectableModel(entry.driverKind, modelSlug, options);
    if (!resolvedModel) return null;
    if (additive && onToggleModel) {
      onToggleModel(instanceId, resolvedModel);
    } else {
      onInstanceModelChange(instanceId, resolvedModel);
    }
    return resolvedModel;
  };

  const modelJumpShortcutContext = {
    terminalFocus: false,
    terminalOpen: props.terminalOpen,
    modelPickerOpen: true,
  } as const;
  const lockedProvider = props.lockedProvider;
  const lockedContinuationGroupKey = props.lockedContinuationGroupKey;
  const canOpenProviderSetup = onOpenProviderSetup !== undefined;

  const entryByInstanceId = new Map(instanceEntries.map((entry) => [entry.instanceId, entry]));
  const matchesLockedProvider = (
    entry: Pick<ProviderInstanceEntry, "driverKind" | "continuationGroupKey">,
  ): boolean => {
    if (lockedProvider === null) return true;
    if (entry.driverKind !== lockedProvider) return false;
    if (!lockedContinuationGroupKey) return true;
    return entry.continuationGroupKey === lockedContinuationGroupKey;
  };

  const flatModels = (() => {
    const out: ModelPickerItem[] = [];
    const seenKeys = new Set<string>();
    for (const [instanceId, models] of modelOptionsByInstance) {
      const entry = entryByInstanceId.get(instanceId);
      if (!entry) {
        continue;
      }
      for (const model of models) {
        const key = modelPickerModelKey(instanceId, model.slug);
        if (
          seenKeys.has(key) ||
          !shouldIncludeModelPickerOption({
            entry,
            option: model,
            activeInstanceId: props.activeInstanceId,
            activeModel: activeModelSlug,
          })
        ) {
          continue;
        }
        seenKeys.add(key);
        out.push({
          slug: model.slug,
          name: model.name,
          ...(model.shortName ? { shortName: model.shortName } : {}),
          ...(model.subProvider ? { subProvider: model.subProvider } : {}),
          ...(model.badge ? { badge: model.badge } : {}),
          ...(model.isLegacy ? { isLegacy: true } : {}),
          ...(model.isUnavailable ? { isUnavailable: true } : {}),
          instanceId,
          driverKind: entry.driverKind,
          instanceDisplayName: entry.displayName,
          ...(entry.accentColor ? { instanceAccentColor: entry.accentColor } : {}),
          ...(entry.acpRegistryAgentId ? { acpRegistryAgentId: entry.acpRegistryAgentId } : {}),
          ...(entry.acpRegistryIconUrl ? { acpRegistryIconUrl: entry.acpRegistryIconUrl } : {}),
          ...(entry.continuationGroupKey
            ? { continuationGroupKey: entry.continuationGroupKey }
            : {}),
        });
      }
    }
    return out;
  })();
  const modelByKey = new Map(
    flatModels.map((model) => [modelPickerModelKey(model.instanceId, model.slug), model] as const),
  );

  const isSearching = searchQuery.trim().length > 0;
  const pickableModels =
    lockedProvider === null ? flatModels : flatModels.filter((m) => matchesLockedProvider(m));
  const instanceOrder = instanceEntries.map((entry) => entry.instanceId);
  const modelRow = (model: ModelPickerItem, inFavorites = false): ListRow => ({
    kind: "model",
    key: inFavorites
      ? modelPickerFavoriteKey(model.instanceId, model.slug)
      : modelPickerModelKey(model.instanceId, model.slug),
    model,
  });

  const favoriteModels = isSearching
    ? []
    : sortProviderModelItems(pickableModels.filter(isFavorite), {
        favoriteModelKeys: favoritesSet,
        groupFavorites: false,
        instanceOrder,
      });

  const rows = ((): ListRow[] => {
    if (isSearching) {
      return rankModelPickerSearch(pickableModels, searchQuery, (model) => ({
        name: model.name,
        ...(model.shortName ? { shortName: model.shortName } : {}),
        ...(model.subProvider ? { subProvider: model.subProvider } : {}),
        driverKind: model.driverKind,
        providerDisplayName: model.instanceDisplayName,
        isFavorite: isFavorite(model),
      })).map((model) => modelRow(model));
    }
    const out: ListRow[] = [];
    if (favoriteModels.length > 0) {
      out.push({
        kind: "header",
        key: FAVORITES_SECTION_KEY,
        label: gt("Favorites"),
        entry: null,
        lockedOut: false,
      });
      for (const model of favoriteModels) out.push(modelRow(model, true));
    }
    const lockedOut: ProviderInstanceEntry[] = [];
    const offersSetup = (entry: ProviderInstanceEntry) =>
      canOpenProviderSetup &&
      isProviderInstancePickerVisible(entry) &&
      matchesLockedProvider(entry) &&
      shouldOfferModelPickerSetup(entry, modelOptionsByInstance.get(entry.instanceId) ?? []);
    const setupCount = instanceEntries.filter(offersSetup).length;
    for (const entry of instanceEntries) {
      if (!isProviderInstancePickerVisible(entry)) continue;
      if (!matchesLockedProvider(entry)) {
        lockedOut.push(entry);
        continue;
      }
      const models = sortProviderModelItems(
        pickableModels.filter((model) => model.instanceId === entry.instanceId),
        { favoriteModelKeys: favoritesSet, groupFavorites: false, instanceOrder: [] },
      );
      const isOlder = (model: ModelPickerItem) => model.isLegacy === true && !isFavorite(model);
      const older = models.filter(isOlder);
      const expanded = expandedLegacyInstances.has(entry.instanceId);
      const section: ListRow[] = [
        ...models.filter((model) => !isOlder(model)),
        ...(expanded ? older : []),
      ].map((model) => modelRow(model));
      // The toggle trails the section, so expanding it reveals older models above it.
      if (older.length > 0) {
        section.push({
          kind: "legacy",
          key: modelPickerLegacySectionKey(entry.instanceId),
          expanded,
        });
      }
      if (offersSetup(entry)) {
        section.push({
          kind: "setup",
          key: `setup:${entry.instanceId}`,
          entry,
          label:
            setupCount > 1
              ? gt("Set up {provider}", { provider: entry.displayName })
              : gt("Open provider setup"),
        });
      }
      const notice = formatProviderUpdateRequiredNotice(entry.snapshot, "", messages, locale);
      if (notice) section.push({ kind: "notice", key: `notice:${entry.instanceId}`, notice });
      if (section.length === 0) continue;
      out.push(
        {
          kind: "header",
          key: `section:${entry.instanceId}`,
          label: entry.displayName,
          entry,
          lockedOut: false,
        },
        ...section,
      );
    }
    for (const entry of lockedOut) {
      out.push({
        kind: "header",
        key: `section:${entry.instanceId}`,
        label: entry.displayName,
        entry,
        lockedOut: true,
      });
    }
    return out;
  })();

  const rowIndexByKey = new Map(rows.map((row, index) => [row.key, index] as const));
  const filteredItemKeys = rows.filter(isHighlightableRow).map((row) => row.key);
  const comboIndexByKey = new Map(filteredItemKeys.map((key, index) => [key, index] as const));
  const headerIndices = isSearching
    ? undefined
    : rows.flatMap((row, index) => (row.kind === "header" ? [index] : []));
  const allItemKeys = [
    ...flatModels.map((model) => modelPickerModelKey(model.instanceId, model.slug)),
    ...favoriteModels.map((model) => modelPickerFavoriteKey(model.instanceId, model.slug)),
    ...new Set(
      flatModels
        .filter((model) => model.isLegacy)
        .map((model) => modelPickerLegacySectionKey(model.instanceId)),
    ),
  ];
  const comboValue = [...selectedModelKeySet].flatMap((key) => {
    const model = modelByKey.get(key);
    return model && isFavorite(model)
      ? [key, modelPickerFavoriteKey(model.instanceId, model.slug)]
      : [key];
  });
  const comboValueSet = new Set(comboValue);

  const updateRequiredNotices = isSearching
    ? instanceEntries.flatMap((entry) => {
        const notice = matchesLockedProvider(entry)
          ? formatProviderUpdateRequiredNotice(entry.snapshot, searchQuery, messages, locale)
          : null;
        return notice ? [{ instanceId: entry.instanceId, notice }] : [];
      })
    : [];

  // Favorites own the jump shortcuts; without any, the first models in the list do.
  const modelJumpModelKeys = rows
    .filter(
      (row): row is Extract<ListRow, { kind: "model" }> =>
        row.kind === "model" &&
        (isSearching || favoriteModels.length === 0 || row.key.startsWith("favorite:")) &&
        !getModelDisabledReason?.(row.model.instanceId, row.model.slug),
    )
    .slice(0, MODEL_PICKER_JUMP_KEYBINDING_COMMANDS.length)
    .map((row) => row.key);
  const modelJumpLabelByKey = modelJumpLabelsByKey(
    modelJumpModelKeys,
    keybindings,
    props.terminalOpen,
  );
  const badgedInstanceIds = new Set(
    instanceEntries
      .filter((entry) => shouldShowInstanceBadge(entry, instanceEntries))
      .map((entry) => entry.instanceId),
  );
  const modelListExtraData = {
    badgedInstanceIds,
    favoritesSet,
    modelJumpLabelByKey,
    activeModelKey,
    selectedModelKeySet,
  };

  // The selected model's row when it is not a favorite, so a long list opens on it.
  const activeIsFavorite =
    activeModel !== undefined &&
    isFavorite({ instanceId: props.activeInstanceId, slug: activeModelSlug });
  const initialScrollIndex =
    activeModelKey && !activeIsFavorite ? rowIndexByKey.get(activeModelKey) : undefined;
  const floatingCardHost = props.cardHost;
  // Under the list, the selected model's options take a fixed share of the popup's height.
  const optionsUnderList = !floatingCardHost && props.controls !== undefined;
  const listHeight =
    isSearching || optionsUnderList
      ? undefined
      : Math.min(
          LIST_MAX_PX,
          rows.reduce((sum, row) => sum + estimatedRowHeight(row), 0) + LIST_PADDING_PX,
        );

  const resolveCardModel = (key: string): CardModel | null => {
    const parsed = parseModelPickerModelKey(key);
    if (!parsed) return null;
    const plainKey = modelPickerModelKey(parsed.instanceId, parsed.slug);
    const model = modelByKey.get(plainKey);
    const entry = entryByInstanceId.get(parsed.instanceId);
    if (!model || !entry) return null;
    return {
      key: plainKey,
      instanceId: model.instanceId,
      slug: model.slug,
      name: model.name,
      driverKind: model.driverKind,
      providerDisplayName: model.instanceDisplayName,
      providerAccentColor: model.instanceAccentColor,
      acpRegistryAgentId: model.acpRegistryAgentId,
      acpRegistryIconUrl: model.acpRegistryIconUrl,
      contextLabel: modelContextLabel(entry, model.slug),
      runsOn: model.subProvider ? `${entry.displayName} · ${model.subProvider}` : entry.displayName,
      isFavorite: isFavorite(model),
    };
  };
  const inlineCardModel = activeModelKey ? resolveCardModel(activeModelKey) : null;

  const scrollToSection = (direction: 1 | -1) => {
    const headers = headerIndices?.filter((index) => {
      const row = rows[index];
      return row?.kind === "header" && !row.lockedOut;
    });
    if (!headers || headers.length === 0) return;
    const highlightedIndex = highlightedModelKeyRef.current
      ? (rowIndexByKey.get(highlightedModelKeyRef.current) ?? -1)
      : -1;
    const current =
      sectionCursorRef.current >= 0
        ? sectionCursorRef.current
        : headers.findLastIndex((index) => index <= highlightedIndex);
    const next =
      current < 0
        ? direction === 1
          ? 0
          : headers.length - 1
        : (current + direction + headers.length) % headers.length;
    sectionCursorRef.current = next;
    const headerIndex = headers[next]!;
    void modelListRef.current?.scrollToIndex({ index: headerIndex, animated: false });
    const header = rows[headerIndex];
    if (header?.kind === "header") announce(header.label, "model-picker");
  };

  const onWindowKeyDown = (event: globalThis.KeyboardEvent) => {
    if (
      event.defaultPrevented ||
      event.repeat ||
      event.isComposing ||
      event.keyCode === 229 ||
      isCommandPaletteOpen()
    ) {
      return;
    }

    const command = resolveShortcutCommand(event, keybindings, {
      platform: navigator.platform,
      context: modelJumpShortcutContext,
    });
    if (command === "modelPicker.previousProvider" || command === "modelPicker.nextProvider") {
      event.preventDefault();
      event.stopPropagation();
      if (isSearching) setSearchQuery("");
      scrollToSection(command === "modelPicker.nextProvider" ? 1 : -1);
      return;
    }
    const jumpIndex = modelPickerJumpIndexFromCommand(command ?? "");
    if (jumpIndex === null) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();

    const targetModelKey = modelJumpModelKeys[jumpIndex];
    if (!targetModelKey) {
      return;
    }
    const model = parseModelPickerModelKey(targetModelKey);
    if (!model) {
      return;
    }
    const selectedSlug = handleModelSelect(model.slug, model.instanceId);
    if (selectedSlug === null) return;
    const name = flatModels.find(
      (item) => item.slug === selectedSlug && item.instanceId === model.instanceId,
    )?.name;
    announce(gt("Model: {name}", { name: name ?? selectedSlug }), "model-picker");
  };

  return (
    <TooltipProvider delay={0}>
      <WindowKeyDownListener onKeyDown={onWindowKeyDown} />
      <div
        ref={pickerContentRef}
        className={cn(
          "relative flex w-screen max-w-80 flex-col overflow-hidden bg-popover",
          optionsUnderList ? "h-91 max-h-(--available-height)" : "max-h-(--available-height)",
        )}
        style={isSearching && searchHeight > 0 ? { height: searchHeight } : undefined}
        data-model-picker-content="true"
      >
        <Combobox<string, boolean>
          inline
          items={allItemKeys}
          filteredItems={filteredItemKeys}
          filter={null}
          autoHighlight
          open
          virtualized
          multiple={onToggleModel !== undefined}
          value={onToggleModel ? comboValue : activeModelKey}
          onItemHighlighted={(modelKey, eventDetails) => {
            highlightedModelKeyRef.current = typeof modelKey === "string" ? modelKey : null;
            if (typeof modelKey !== "string") return;
            sectionCursorRef.current = -1;
            // The pointer moves the card itself (see the list), and the open's own highlight of the
            // first row leaves it on the selected model.
            if (eventDetails.reason === "keyboard" || isSearching) cardStore.show(modelKey);
            if (eventDetails.reason === "keyboard") {
              const index = rowIndexByKey.get(modelKey);
              if (index !== undefined) {
                void modelListRef.current?.scrollIndexIntoView?.({ index, animated: false });
              }
            }
          }}
          onValueChange={(value, details) => {
            const modelKey = Array.isArray(value)
              ? (value.find((key) => !comboValueSet.has(key)) ??
                comboValue.find((key) => !value.includes(key)))
              : value;
            if (typeof modelKey !== "string") {
              return;
            }
            const legacyInstanceId = parseModelPickerLegacySectionKey(modelKey);
            if (legacyInstanceId) {
              toggleLegacySection(legacyInstanceId);
              return;
            }
            const model = parseModelPickerModelKey(modelKey);
            if (model) {
              handleModelSelect(
                model.slug,
                model.instanceId,
                "shiftKey" in details.event && details.event.shiftKey === true,
              );
            }
          }}
        >
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
            <div className="shrink-0 border-b border-border p-1">
              <ComboboxPrimitive.Input
                ref={searchInputRef}
                placeholder={gt("Search models")}
                aria-label={gt("Search models")}
                className="h-7 w-full min-w-0 bg-transparent px-2 text-sm text-foreground outline-none placeholder:text-muted-foreground"
                value={searchQuery}
                onChange={(e) => {
                  if (!isSearching) setSearchHeight(pickerContentRef.current?.offsetHeight ?? 0);
                  setSearchQuery(e.target.value);
                }}
                onKeyDown={(e) => {
                  if (e.nativeEvent.isComposing || e.keyCode === 229) {
                    (
                      e as typeof e & { preventBaseUIHandler?: () => void }
                    ).preventBaseUIHandler?.();
                    e.stopPropagation();
                    return;
                  }
                  if (e.key === "Escape") {
                    e.preventDefault();
                    e.stopPropagation();
                    onRequestClose?.();
                    return;
                  }
                  if (
                    props.onFocusEffort &&
                    e.key === "ArrowRight" &&
                    !e.altKey &&
                    !e.ctrlKey &&
                    !e.metaKey &&
                    !e.shiftKey &&
                    e.currentTarget.selectionStart === searchQuery.length
                  ) {
                    e.preventDefault();
                    e.stopPropagation();
                    // Options show for the selected model, so a highlighted model is picked
                    // first; the owning popup then moves focus to that model's levels.
                    const highlighted = highlightedModelKeyRef.current
                      ? parseModelPickerModelKey(highlightedModelKeyRef.current)
                      : null;
                    const pickedNew =
                      highlighted !== null &&
                      modelPickerModelKey(highlighted.instanceId, highlighted.slug) !==
                        activeModelKey &&
                      handleModelSelect(highlighted.slug, highlighted.instanceId) !== null;
                    if (pickedNew && highlightedModelKeyRef.current) {
                      cardStore.show(highlightedModelKeyRef.current);
                    }
                    if (!pickedNew) props.onFocusEffort();
                    return;
                  }
                  if (e.key === "Enter" && highlightedModelKeyRef.current) {
                    (
                      e as typeof e & { preventBaseUIHandler?: () => void }
                    ).preventBaseUIHandler?.();
                    e.preventDefault();
                    e.stopPropagation();
                    const legacyInstanceId = parseModelPickerLegacySectionKey(
                      highlightedModelKeyRef.current,
                    );
                    if (legacyInstanceId) {
                      toggleLegacySection(legacyInstanceId);
                      return;
                    }
                    const model = parseModelPickerModelKey(highlightedModelKeyRef.current);
                    if (model) {
                      handleModelSelect(model.slug, model.instanceId, e.shiftKey);
                    }
                    return;
                  }
                  e.stopPropagation();
                }}
                onMouseDown={(e) => e.stopPropagation()}
                onTouchStart={(e) => e.stopPropagation()}
              />
            </div>

            <ModelPickerList
              listRef={modelListRef}
              rows={rows}
              height={listHeight}
              stickyHeaderIndices={headerIndices}
              initialScrollIndex={initialScrollIndex}
              extraData={modelListExtraData}
              onScroll={cardStore.place}
              onPointerMove={(event) => {
                const key = (event.target as Element)
                  .closest("[data-model-picker-row]")
                  ?.getAttribute("data-model-picker-row");
                if (key && parseModelPickerModelKey(key)) cardStore.show(key);
              }}
              renderItem={({ item: row }) => {
                switch (row.kind) {
                  case "header":
                    return <SectionHeader row={row} />;
                  case "setup":
                    return (
                      <ProviderSetupRow
                        entry={row.entry}
                        label={row.label}
                        onRequestClose={onRequestClose}
                        onOpenProviderSetup={onOpenProviderSetup}
                      />
                    );
                  case "notice":
                    return (
                      <UpdateRequiredNotice notice={row.notice} onRequestClose={onRequestClose} />
                    );
                  case "legacy":
                    return (
                      <ModelPickerRowItem
                        index={comboIndexByKey.get(row.key) ?? -1}
                        value={row.key}
                        aria-expanded={row.expanded}
                        className="text-muted-foreground data-highlighted:text-foreground"
                      >
                        {row.expanded
                          ? gt("Show less", { $context: "collapses the list of older models" })
                          : gt("Show more", { $context: "expands the list of older models" })}
                      </ModelPickerRowItem>
                    );
                  case "model": {
                    const { model } = row;
                    const plainKey = modelPickerModelKey(model.instanceId, model.slug);
                    return (
                      <ModelListRow
                        key={row.key}
                        index={comboIndexByKey.get(row.key) ?? -1}
                        rowKey={row.key}
                        name={model.name}
                        subProvider={model.subProvider}
                        driverKind={model.driverKind}
                        providerDisplayName={model.instanceDisplayName}
                        providerAccentColor={model.instanceAccentColor}
                        acpRegistryAgentId={model.acpRegistryAgentId}
                        acpRegistryIconUrl={model.acpRegistryIconUrl}
                        isSelected={selectedModelKeySet.has(plainKey)}
                        showInstanceBadge={badgedInstanceIds.has(model.instanceId)}
                        showNewBadge={model.badge === "new"}
                        unavailable={model.isUnavailable === true}
                        contextLabel={modelContextLabel(
                          entryByInstanceId.get(model.instanceId),
                          model.slug,
                        )}
                        jumpLabel={modelJumpLabelByKey.get(row.key) ?? null}
                        disabledReason={
                          getModelDisabledReason?.(model.instanceId, model.slug) ?? null
                        }
                      />
                    );
                  }
                }
              }}
            />
            <ComboboxEmpty className="empty:h-0">
              {rows.length > 0
                ? null
                : workspaceModelsScanning
                  ? gt("Searching project models...")
                  : gt("No models found")}
            </ComboboxEmpty>
            {updateRequiredNotices.map(({ instanceId, notice }) => (
              <UpdateRequiredNotice
                key={instanceId}
                notice={notice}
                onRequestClose={onRequestClose}
                className="shrink-0 border-t border-border px-3"
              />
            ))}
            {props.listFooter}
          </div>
        </Combobox>
        {floatingCardHost ? (
          <FloatingModelCard
            store={cardStore}
            host={floatingCardHost}
            fallbackKey={activeModelKey}
            resolve={resolveCardModel}
            activeModelKey={activeModelKey}
            controls={props.controls}
            onToggleFavorite={toggleFavorite}
          />
        ) : inlineCardModel ? (
          <ModelDetailCard
            floating={false}
            model={inlineCardModel}
            controls={props.controls}
            onToggleFavorite={() =>
              toggleFavorite(inlineCardModel.instanceId, inlineCardModel.slug)
            }
          />
        ) : null}
      </div>
    </TooltipProvider>
  );
});
