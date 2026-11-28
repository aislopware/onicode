import { msg, T, useGT, useLocale, useMessages, Var } from "gt-react-native";
import { managedCapabilityDenial } from "@onicode/client-runtime/contract-labels";
import * as Dialog from "../../components/ui/Dialog";
import { RegistryContext, useAtomValue } from "@effect/atom-react";
import { AsyncResult } from "effect/reactivity";
import {
  AuthOrchestrationOperateScope,
  clampManagedRuntimeMode,
  resolveProjectProviderInstanceEnabled,
} from "@onicode/contracts";
import { readEnvironmentScope, usePreparedConnection } from "../../state/session";
import type {
  EnvironmentId,
  ProjectId,
  ScheduledTask,
  ScheduledTaskThreadMode,
  ScheduledTaskUpsertInput,
} from "@onicode/contracts";
import {
  MAX_WEBHOOK_FILTERS,
  resolveEnvironmentMachineKind,
  scheduledTaskThreadMode,
  trimAsciiWhitespace,
} from "@onicode/contracts";
import { useSelectWorkspaceDetail } from "../layout/workspace/useSelectWorkspaceDetail";
import type { MenuAction } from "@react-native-menu/menu";
import { DateTimePicker } from "@expo/ui/community/datetime-picker";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
  type AtomCommandResult,
} from "@onicode/client-runtime/state/runtime";
import { normalizeProjectPathForDispatch, resolvePastedPath } from "@onicode/shared/path";
import {
  refusedScheduledTask,
  scheduledTaskExpected,
} from "@onicode/client-runtime/scheduled-task-draft";
import {
  DEFAULT_WEBHOOK_PROMPT,
  emptyWebhookFilterDraft,
  parseWebhookFilterDrafts,
  removeWebhookFilterDraft,
  WEBHOOK_FILTER_OPS,
  type WebhookFilterDraft,
} from "@onicode/client-runtime/scheduled-task-webhook";
import { webhookAddress } from "@onicode/client-runtime/webhook-address";
import { scheduledTaskWorktreeBaseRef } from "@onicode/client-runtime/scheduled-task-worktree";
import { scheduleTimeZoneLabel } from "@onicode/client-runtime/scheduled-task-time-zone";
import type { MessageResolver } from "../../i18n/messages";
import { useContext, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import {
  useFocusEffect,
  useNavigation,
  usePreventRemove,
  type NavigationAction,
} from "@react-navigation/native";
import type { NativeStackNavigationProp } from "@react-navigation/native-stack";
import { AppState, Platform, Pressable, TextInput as RNTextInput, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { Button } from "../../components/ui/Button";
import { AppText as Text } from "../../components/ui/AppText";
import { SymbolView } from "../../components/ui/AppSymbol";
import {
  EnvironmentMachineSymbol,
  ENVIRONMENT_MACHINE_SYMBOLS,
} from "../../components/EnvironmentMachineSymbol";
import { Menu } from "../../components/ui/Menu";
import type { ComposerEditorSelection } from "../../components/ComposerEditor";
import { ScreenScrollView as ScrollView } from "../../components/ui/ScreenScrollView";
import { SegmentedControl } from "../../components/ui/SegmentedControl";
import { ThemedSwitch } from "../../components/ui/ThemedSwitch";
import { tryCopyTextWithHaptic } from "../../lib/copyTextWithHaptic";
import { buildModelOptions } from "../../lib/modelOptions";
import { withImplicitFastModeSelection } from "../../lib/providerOptions";
import { useProjects, useEnvironmentServerConfig } from "../../state/entities";
import { useEnvironmentQuery } from "../../state/query";
import { usePaginatedBranches } from "../../state/queries";
import { serverEnvironment } from "../../state/server";
import { useAtomCommand } from "../../state/use-atom-command";
import { resolveNewTaskBranchLabel } from "../threads/new-task-context-presentation";
import { useSystem24HourClock } from "../threads/useMessageTimeFormatter";
import { useVoiceInputController } from "../voice-input/useVoiceInputController";
import { ScheduledTaskPromptField } from "./components/ScheduledTaskPromptField";
import { useSettingsEnvironmentMenu } from "./components/SettingsEnvironmentFilterHeader";
import { SettingsScreen } from "./components/SettingsScreen";
import { SettingsSection } from "./components/SettingsSection";
import { useSettingsEnvironmentFilter, type SettingsTarget } from "./settings-environment-filter";
import {
  changeDraftProject,
  editDraft,
  parseIntervalMinutes,
  rebaseScheduledTaskDraft,
  scheduledTaskDefaultModel,
  scheduleFromDraft,
  unbindDraftFromThread,
  webhookScheduleProblems,
  type ScheduledTaskDraft as Draft,
} from "./scheduledTaskDraft";
import { settingsTargetsForProject } from "./settings-environment-filter.logic";
import {
  useScheduledTaskDestinationLabel,
  useScheduledTaskEditor,
  useScheduledTaskThread,
} from "./scheduled-task-editor";
import {
  discardScheduledTaskEditor,
  markScheduledTaskEditorSaved,
  isScheduledTaskEditorSaving,
  scheduledTaskEditorBlocksRemoval,
} from "./scheduled-task-editor-state";
import {
  formatNextScheduledTaskRun,
  formatScheduledRunTime,
  formatScheduledTaskInterval,
  formatScheduledTimeOfDay,
} from "./scheduledTaskPresentation";

type ScheduledTaskRoutes = {
  SettingsScheduledTaskNew: undefined;
  SettingsScheduledTaskEdit: undefined;
  SettingsScheduledTaskModel: undefined;
  SettingsScheduledTaskBranch: undefined;
  SettingsScheduledTaskThread: undefined;
  SettingsScheduledTaskDeliveries: { readonly environmentId: string; readonly taskId: string };
};

const DAYS = [1, 2, 3, 4, 5, 6, 0].map((index) => ({ index }));

// 2024-01-07 was a Sunday, so day index 0 to 6 maps onto Sunday to Saturday.
function weekdayLabel(index: number, locale: string): string {
  return new Intl.DateTimeFormat(locale, { weekday: "short", timeZone: "UTC" }).format(
    Date.UTC(2024, 0, 7 + index),
  );
}

const THREAD_MODE_LABELS: Record<ScheduledTaskThreadMode, string> = {
  new_thread: msg("New thread each run"),
  continue: msg("One thread, same conversation"),
  fresh_session: msg("One thread, fresh session"),
};

function describeSchedule(
  task: ScheduledTask,
  timeZone: string | undefined,
  resolve: MessageResolver,
  locale: string,
  is24Hour: boolean | undefined,
): string {
  if (task.schedule.type === "webhook") return resolve(msg("On webhook"));
  if (task.schedule.type === "once") {
    const runAt = new Date(task.schedule.runAt);
    return resolve(
      msg("Once, {date} at {time}", {
        date: formatRunAtDate(runAt),
        time: formatScheduledRunTime(runAt, is24Hour),
      }),
    );
  }
  if (task.schedule.type === "interval")
    return formatScheduledTaskInterval(task.schedule.everyMs, resolve);
  const days = task.schedule.weekdays?.length
    ? repeatLabel(task.schedule.weekdays, resolve, locale)
    : resolve(msg("Every day"));
  const zone = scheduleTimeZoneLabel(timeZone, task.nextRunAt);
  const time = formatScheduledTimeOfDay(task.schedule.timeOfDay, is24Hour);
  return zone === undefined
    ? resolve(msg("{days} at {time}", { days, time }))
    : resolve(msg("{days} at {time} {zone}", { days, time, zone }));
}

function formatRunAtDate(value: Date): string {
  return value.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
}

// A fixed date with no DST change, so today's gap cannot move the picker to another hour.
function timePickerValue(value: string): Date {
  const [hours, minutes] = value.split(":").map(Number);
  return new Date(
    2000,
    0,
    1,
    Number.isInteger(hours) ? (hours ?? 9) : 9,
    Number.isInteger(minutes) ? (minutes ?? 0) : 0,
  );
}

function isWeekdays(weekdays: ReadonlyArray<number>): boolean {
  const days = new Set(weekdays);
  return days.size === 5 && [1, 2, 3, 4, 5].every((day) => days.has(day));
}

function repeatLabel(
  weekdays: ReadonlyArray<number>,
  resolve: MessageResolver,
  locale: string,
): string {
  const days = new Set(weekdays);
  if (days.size === 7) return resolve(msg("Every day"));
  if (isWeekdays(weekdays)) return resolve(msg("Weekdays"));
  return (
    DAYS.filter((day) => days.has(day.index))
      .map((day) => weekdayLabel(day.index, locale))
      .join(", ") || resolve(msg("Choose days"))
  );
}

function FormField(props: {
  readonly label: string;
  readonly value: string;
  readonly onChange: (value: string) => void;
  readonly keyboardType?: "decimal-pad" | "number-pad";
  readonly disabled?: boolean;
  readonly placeholder?: string;
  readonly borderTop?: boolean;
  /** Typed as entered: paths, header names, templates and secrets. */
  readonly verbatim?: boolean;
  readonly secure?: boolean;
}) {
  return (
    <View
      className={
        props.borderTop ? "gap-2 border-t border-border-subtle px-4 py-3" : "gap-2 px-4 py-3"
      }
    >
      <Text className="text-sm text-foreground-muted">{props.label}</Text>
      <RNTextInput
        accessibilityLabel={props.label}
        value={props.value}
        readOnly={props.disabled}
        onChangeText={props.onChange}
        textAlignVertical="center"
        keyboardType={props.keyboardType}
        autoCapitalize={props.verbatim || props.secure ? "none" : undefined}
        autoCorrect={props.verbatim || props.secure ? false : undefined}
        secureTextEntry={props.secure}
        placeholder={props.placeholder}
        placeholderTextColorClassName="accent-foreground-muted"
        className="min-h-8 font-sans text-base text-foreground"
      />
    </View>
  );
}

function SelectRow(props: {
  readonly label: string;
  readonly value: string;
  readonly valueIcon?: ReactNode;
  readonly actions: MenuAction[];
  readonly onSelect: (id: string) => void;
  readonly borderTop?: boolean;
}) {
  const gt = useGT();
  const value = (
    <View className="min-w-0 flex-1 flex-row items-center justify-end gap-2">
      {props.valueIcon}
      <Text
        textRole="footnote"
        className="shrink text-right text-foreground-muted"
        numberOfLines={1}
      >
        {props.value}
      </Text>
    </View>
  );
  if (props.actions.length === 0) {
    return (
      <View
        className={
          props.borderTop
            ? "min-h-14 flex-row items-center gap-3 border-t border-border-subtle px-4 py-3"
            : "min-h-14 flex-row items-center gap-3 px-4 py-3"
        }
      >
        <Text textRole="body" className="text-foreground">
          {props.label}
        </Text>
        {value}
      </View>
    );
  }
  return (
    <Menu
      actions={props.actions}
      onPressAction={({ nativeEvent }) => props.onSelect(nativeEvent.event)}
    >
      <Pressable
        accessibilityRole="button"
        accessibilityLabel={gt("{label}, {value}", { label: props.label, value: props.value })}
        className={
          props.borderTop
            ? "min-h-14 flex-row items-center gap-3 border-t border-border-subtle px-4 py-3 active:opacity-70"
            : "min-h-14 flex-row items-center gap-3 px-4 py-3 active:opacity-70"
        }
      >
        <Text textRole="body" className="text-foreground">
          {props.label}
        </Text>
        {value}
        <SymbolView
          name="chevron.down"
          size={14}
          tintColorClassName="accent-chevron"
          type="monochrome"
        />
      </Pressable>
    </Menu>
  );
}

function WebhookFilterRows(props: {
  readonly filters: ReadonlyArray<WebhookFilterDraft>;
  readonly disabled: boolean;
  readonly onChange: (
    update: (filters: ReadonlyArray<WebhookFilterDraft>) => ReadonlyArray<WebhookFilterDraft>,
  ) => void;
}) {
  const gt = useGT();
  const m = useMessages();
  const update = (id: WebhookFilterDraft["id"], patch: Partial<WebhookFilterDraft>) =>
    props.onChange((filters) =>
      filters.map((filter) => (filter.id === id ? { ...filter, ...patch } : filter)),
    );
  return (
    <>
      {props.filters.map((filter, index) => (
        <View key={filter.id}>
          <View
            className={
              index === 0
                ? "flex-row items-center gap-3 px-4 pt-3"
                : "flex-row items-center gap-3 border-t border-border-subtle px-4 pt-3"
            }
          >
            <Text className="min-w-0 flex-1 text-sm text-foreground-muted">
              <T>
                Filter <Var>{index + 1}</Var>
              </T>
            </Text>
            <Pressable
              accessibilityRole="button"
              accessibilityLabel={gt("Remove filter {number}", { number: index + 1 })}
              disabled={props.disabled}
              hitSlop={8}
              onPress={() =>
                props.onChange((filters) => removeWebhookFilterDraft(filters, filter.id))
              }
              className="active:opacity-70"
            >
              <SymbolView name="trash" size={16} tintColorClassName="accent-danger-foreground" />
            </Pressable>
          </View>
          <FormField
            label={gt("Path", { $context: "webhook payload field path" })}
            value={filter.path}
            placeholder="body.action"
            disabled={props.disabled}
            verbatim
            onChange={(path) => update(filter.id, { path })}
          />
          <SelectRow
            label={gt("Test", { $context: "noun, the comparison a webhook filter applies" })}
            value={m(WEBHOOK_FILTER_OPS.find((option) => option.op === filter.op)?.label ?? "")}
            actions={WEBHOOK_FILTER_OPS.map((option) => ({
              id: option.op,
              title: m(option.label),
              state: option.op === filter.op ? ("on" as const) : undefined,
            }))}
            onSelect={(id) => {
              const known = WEBHOOK_FILTER_OPS.find((option) => option.op === id);
              if (known) update(filter.id, { op: known.op });
            }}
          />
          {filter.op === "exists" ? null : (
            <FormField
              label={filter.op === "in" ? gt("Values, comma-separated") : gt("Value")}
              value={filter.value}
              // oxlint-disable-next-line onicode/no-untranslated-copy -- webhook action values
              placeholder={filter.op === "in" ? "opened, reopened" : "opened"}
              disabled={props.disabled}
              verbatim
              onChange={(value) => update(filter.id, { value })}
            />
          )}
        </View>
      ))}
      {props.filters.length < MAX_WEBHOOK_FILTERS ? (
        <Pressable
          accessibilityRole="button"
          disabled={props.disabled}
          onPress={() =>
            props.onChange((filters) =>
              filters.length < MAX_WEBHOOK_FILTERS
                ? [...filters, emptyWebhookFilterDraft()]
                : filters,
            )
          }
          className={
            props.filters.length === 0
              ? "min-h-14 flex-row items-center gap-3 px-4 py-3 active:opacity-70"
              : "min-h-14 flex-row items-center gap-3 border-t border-border-subtle px-4 py-3 active:opacity-70"
          }
        >
          <Text textRole="body" className="min-w-0 flex-1 text-foreground">
            <T>Add filter</T>
          </Text>
          <SymbolView name="plus" size={16} tintColorClassName="accent-icon" />
        </Pressable>
      ) : null}
    </>
  );
}

function PickerRow(props: {
  readonly label: string;
  readonly value: string;
  readonly onPress: () => void;
  readonly disabled?: boolean;
  readonly borderTop?: boolean;
}) {
  const gt = useGT();
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={gt("{label}, {value}", { label: props.label, value: props.value })}
      accessibilityState={{ disabled: props.disabled }}
      disabled={props.disabled}
      onPress={props.onPress}
      className={
        props.borderTop
          ? "min-h-14 flex-row items-center gap-3 border-t border-border-subtle px-4 py-3 active:opacity-70 disabled:opacity-50"
          : "min-h-14 flex-row items-center gap-3 px-4 py-3 active:opacity-70 disabled:opacity-50"
      }
    >
      <Text textRole="body" className="text-foreground">
        {props.label}
      </Text>
      <Text
        textRole="footnote"
        className="min-w-0 flex-1 text-right text-foreground-muted"
        numberOfLines={1}
      >
        {props.value}
      </Text>
      <SymbolView
        name="chevron.right"
        size={14}
        tintColorClassName="accent-chevron"
        type="monochrome"
      />
    </Pressable>
  );
}

export function SettingsScheduledTasksRouteScreen() {
  const gt = useGT();
  const [now, setNow] = useState(Date.now);
  useFocusEffect(() => {
    const updateNow = () => setNow(Date.now());
    updateNow();
    const timer = setInterval(updateNow, 60_000);
    const subscription = AppState.addEventListener("change", (state) => {
      if (state === "active") updateNow();
    });
    return () => {
      clearInterval(timer);
      subscription.remove();
    };
  });
  const { availableTargets, selectedTargets, selectedProjectKey, projectGroups } =
    useSettingsEnvironmentFilter();
  const selectedGroup = projectGroups.find((group) => group.key === selectedProjectKey);
  const visibleEnvironments = settingsTargetsForProject(
    selectedTargets,
    selectedProjectKey === null ? null : selectedGroup,
  );
  const { startEditor, resetEditor } = useScheduledTaskEditor();
  const navigation = useNavigation<NativeStackNavigationProp<ScheduledTaskRoutes>>();
  const insets = useSafeAreaInsets();
  const environmentMenu = useSettingsEnvironmentMenu();
  const newTask = () => {
    if (visibleEnvironments.length === 0) return;
    resetEditor();
    navigation.navigate("SettingsScheduledTaskNew");
  };

  return (
    <SettingsScreen
      title={gt("Scheduled tasks")}
      actions={[
        {
          accessibilityLabel: gt("New task"),
          icon: "plus",
          disabled: visibleEnvironments.length === 0,
          onPress: newTask,
        },
      ]}
      menus={[environmentMenu]}
    >
      <ScrollView
        keyboardShouldPersistTaps="handled"
        keyboardDismissMode="on-drag"
        contentInsetAdjustmentBehavior="automatic"
        showsVerticalScrollIndicator={false}
        className="flex-1"
        contentContainerClassName="gap-5 px-5 pt-4"
        contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 18) + 18 }}
      >
        {visibleEnvironments.length > 0 ? (
          visibleEnvironments.map((environment) => (
            <EnvironmentTasks
              key={environment.environmentId}
              environment={environment}
              now={now}
              projectIds={
                selectedProjectKey === null
                  ? null
                  : (selectedGroup?.members
                      .filter(
                        (member) => member.project.environmentId === environment.environmentId,
                      )
                      .map((member) => member.project.id) ?? [])
              }
              onEdit={(task) => {
                startEditor({
                  environmentId: environment.environmentId,
                  environmentLabel: environment.label,
                  draft: editDraft(task),
                });
                navigation.navigate("SettingsScheduledTaskEdit");
              }}
            />
          ))
        ) : (
          <Text className="px-2 text-base text-foreground-muted">
            {availableTargets.length === 0
              ? gt("Connect an environment to view and create scheduled tasks.")
              : gt("No environments match these filters. Change the filter above.")}
          </Text>
        )}
      </ScrollView>
    </SettingsScreen>
  );
}

// The "New task" action and takeScheduledTaskNewLink clear the editor before
// this screen opens, so it starts from the default target.
export function SettingsScheduledTaskNewRouteScreen() {
  const gt = useGT();
  return <SettingsScheduledTaskEditorScreen title={gt("New scheduled task")} />;
}

export function SettingsScheduledTaskEditRouteScreen() {
  const gt = useGT();
  return <SettingsScheduledTaskEditorScreen title={gt("Edit scheduled task")} />;
}

function SettingsScheduledTaskEditorScreen({ title }: { readonly title: string }) {
  const gt = useGT();
  const {
    editor,
    voiceOwnerKey,
    readEditor,
    setEditor,
    hasChanges,
    saving,
    setSaving,
    draftForEnvironment,
  } = useScheduledTaskEditor();
  const { availableTargets } = useSettingsEnvironmentFilter();
  const navigation = useNavigation();
  const insets = useSafeAreaInsets();
  // Set once the save lands, so the removal guard lets the editor close.
  const saved = useRef(false);
  const ownerKey = editor ? `${voiceOwnerKey}:${editor.environmentId}` : null;
  const prompt = editor?.draft.prompt ?? "";
  const [selectionState, setSelectionState] = useState<{
    readonly ownerKey: string | null;
    readonly selection: ComposerEditorSelection;
  } | null>(null);
  const selection =
    selectionState?.ownerKey === ownerKey
      ? selectionState.selection
      : { start: prompt.length, end: prompt.length };
  const setSelection = (next: ComposerEditorSelection) =>
    setSelectionState({ ownerKey, selection: next });
  const setPrompt = (next: string) =>
    setEditor((current) =>
      current
        ? {
            ...current,
            draft: { ...current.draft, prompt: next },
          }
        : current,
    );
  const voiceInput = useVoiceInputController({
    ownerKey,
    label: editor?.draft.title.trim() || title,
    selection,
    readDraftMessage: () => {
      const current = readEditor();
      if (current?.environmentId !== editor?.environmentId) return null;
      return current?.draft.prompt ?? null;
    },
    onChangeSelection: setSelection,
    onChangeDraftMessage: (text) => {
      if (readEditor()?.environmentId === editor?.environmentId) setPrompt(text);
    },
    disabled: saving,
  });
  const preventRemove = hasChanges || saving;
  const guardRemoval = (action: NavigationAction) => {
    if (saved.current || !scheduledTaskEditorBlocksRemoval()) {
      navigation.dispatch(action);
      return;
    }
    if (isScheduledTaskEditorSaving()) {
      Dialog.alert(gt("Saving task"), gt("Wait for the task to finish saving before leaving."));
      return;
    }
    Dialog.alert(gt("Discard changes?"), gt("Your unsaved changes will be lost."), [
      { text: gt("Keep editing"), style: "cancel" },
      {
        text: gt("Discard changes"),
        style: "destructive",
        onPress: () => {
          if (voiceInput.isBusy) voiceInput.cancel();
          discardScheduledTaskEditor();
          navigation.dispatch(action);
        },
      },
    ]);
  };
  usePreventRemove(preventRemove, ({ data }) => guardRemoval(data.action));
  const latestGuard = useRef({ preventRemove, guardRemoval });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- usePreventRemove swaps its guard in a layout effect; this one swaps with it so only one of them answers.
  useLayoutEffect(() => {
    latestGuard.current = { preventRemove, guardRemoval };
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- subscribes to navigation removal: a link landing in Save's or an edit's batch beats the guard above to render.
  useEffect(
    () =>
      navigation.addListener("beforeRemove", (event) => {
        const guard = latestGuard.current;
        if (guard.preventRemove || saved.current || !scheduledTaskEditorBlocksRemoval()) return;
        event.preventDefault();
        guard.guardRemoval(event.data.action);
      }),
    [navigation],
  );
  return (
    <SettingsScreen
      title={title}
      // Native Back pops before removal guards; a bar action lets the guard run first.
      guardBack={preventRemove}
    >
      <ScrollView
        keyboardShouldPersistTaps="handled"
        keyboardDismissMode="on-drag"
        contentInsetAdjustmentBehavior="automatic"
        showsVerticalScrollIndicator={false}
        className="flex-1"
        contentContainerClassName="gap-5 px-5 pt-4"
        contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 18) + 18 }}
      >
        {editor ? (
          <TaskForm
            key={editor.environmentId}
            environmentId={editor.environmentId}
            environmentLabel={
              availableTargets.find((target) => target.environmentId === editor.environmentId)
                ?.label ?? editor.environmentLabel
            }
            availableTargets={availableTargets}
            draft={editor.draft}
            saving={saving}
            setSaving={setSaving}
            dictationPending={voiceInput.blocksSubmission}
            promptField={
              <ScheduledTaskPromptField
                value={prompt}
                onChange={setPrompt}
                selection={selection}
                onChangeSelection={setSelection}
                disabled={saving}
                voiceInput={voiceInput}
              />
            }
            setDraft={(update) => {
              if (!saving)
                setEditor((current) =>
                  current ? { ...current, draft: update(current.draft) } : current,
                );
            }}
            setEnvironmentDraft={(update) => {
              if (!saving)
                setEditor((current) =>
                  current && current.environmentId === editor.environmentId
                    ? { ...current, draft: update(current.draft) }
                    : current,
                );
            }}
            onSaved={() => {
              // An edit made while the save ran keeps the editor open with it unsaved.
              if (!markScheduledTaskEditorSaved(editor)) return;
              saved.current = true;
              if (navigation.isFocused()) navigation.goBack();
            }}
            onChangeEnvironment={(target) => {
              setEditor((current) =>
                !current || target.environmentId === current.environmentId
                  ? current
                  : {
                      environmentId: target.environmentId,
                      environmentLabel: target.label,
                      draft: {
                        ...draftForEnvironment(target.environmentId),
                        title: current.draft.title,
                        prompt: current.draft.prompt,
                        schedule: current.draft.schedule,
                        enabled: current.draft.enabled,
                      },
                    },
              );
            }}
          />
        ) : (
          <Text className="px-2 text-base text-foreground-muted">
            {availableTargets.length === 0
              ? gt("Connect an environment to create a scheduled task.")
              : gt(
                  "No environments match the current filters. Change the filters to create a task.",
                )}
          </Text>
        )}
      </ScrollView>
    </SettingsScreen>
  );
}

function TaskForm({
  environmentId,
  environmentLabel,
  availableTargets,
  draft,
  saving,
  setSaving,
  dictationPending,
  promptField,
  setDraft,
  setEnvironmentDraft,
  onSaved,
  onChangeEnvironment,
}: {
  readonly environmentId: EnvironmentId;
  readonly environmentLabel: string;
  readonly availableTargets: readonly SettingsTarget[];
  readonly draft: Draft;
  readonly saving: boolean;
  readonly setSaving: (saving: boolean) => void;
  readonly dictationPending: boolean;
  readonly promptField: ReactNode;
  readonly setDraft: (update: (current: Draft) => Draft) => void;
  readonly setEnvironmentDraft: (update: (current: Draft) => Draft) => void;
  readonly onSaved: () => void;
  readonly onChangeEnvironment: (target: SettingsTarget) => void;
}) {
  const gt = useGT();
  const m = useMessages();
  const locale = useLocale();
  const is24Hour = useSystem24HourClock();
  const timeOfDayLabel = formatScheduledTimeOfDay(draft.schedule.timeOfDay, is24Hour);
  const { readEditor } = useScheduledTaskEditor();
  const navigation = useNavigation<NativeStackNavigationProp<ScheduledTaskRoutes>>();
  const tasksAtom = serverEnvironment.scheduledTasksLive({ environmentId, input: {} });
  const tasks = useEnvironmentQuery(tasksAtom);
  const registry = useContext(RegistryContext);
  // Pinned first. The chosen project stays listed after it is archived.
  const projects = useProjects()
    .filter(
      (project) =>
        project.environmentId === environmentId &&
        (project.archivedAt == null ||
          project.id === draft.projectId ||
          project.id === draft.task?.projectId),
    )
    .sort((left, right) => Number(right.pinnedAt != null) - Number(left.pinnedAt != null));
  const automaticBaseRef = draft.workspace === "worktree" && !trimAsciiWhitespace(draft.baseRef);
  const automaticBaseRefCwd = automaticBaseRef
    ? (projects.find((project) => project.id === draft.projectId)?.workspaceRoot ?? null)
    : null;
  const automaticBaseRefs = usePaginatedBranches({
    environmentId,
    cwd: automaticBaseRefCwd,
    query: "",
  });
  const worktreeBaseRef = scheduledTaskWorktreeBaseRef(draft.baseRef, automaticBaseRefs.refs);
  // Until the project's refs load, the automatic base would save as `main`. A failed lookup falls back to it.
  const automaticBaseRefLoading =
    automaticBaseRefCwd !== null &&
    automaticBaseRefs.data === null &&
    automaticBaseRefs.error === null;
  const config = useEnvironmentServerConfig(environmentId);
  const destinationLabel = useScheduledTaskDestinationLabel(environmentId, draft.threadId);
  const boundThread = useScheduledTaskThread(environmentId, draft.threadId);
  // Bound runs use the thread's current model, so the locked row shows that one.
  const shownSelection =
    draft.threadId === null ? draft.modelSelection : (boundThread?.modelSelection ?? null);
  const modelOptions = buildModelOptions(config, null, undefined, draft.projectId);
  // Existing webhook tasks keep the option so they can still switch to a timer.
  const webhookDenial = managedCapabilityDenial(config?.managedPolicy, "webhookTasks", m);
  const webhookBlocked =
    draft.schedule.mode === "webhook" && webhookDenial !== undefined && draft.enabled;
  // The live row, for rotated URLs; null once a loaded list no longer has the task.
  const liveTask = tasks.data
    ? (tasks.data.tasks.find((task) => task.id === draft.task?.id) ?? null)
    : draft.task;
  const canOperate = useAtomValue(
    serverEnvironment.upsertScheduledTask.permissionAtom(environmentId),
  );
  const upsert = useAtomCommand(serverEnvironment.upsertScheduledTask, {
    label: "scheduled task upsert",
    reportFailure: false,
  });
  const submissionPending = useRef(false);
  const [timePickerOpen, setTimePickerOpen] = useState(false);
  const [runAtPicker, setRunAtPicker] = useState<"date" | "time" | null>(null);
  const taskMissing =
    draft.task !== null &&
    tasks.data !== null &&
    !tasks.data.tasks.some((task) => task.id === draft.task?.id);
  const runAtZone = scheduleTimeZoneLabel(tasks.data?.timeZone);
  const environmentUnavailable = !availableTargets.some(
    (target) => target.environmentId === environmentId,
  );

  const failure = (title: string, result: AtomCommandResult<unknown, unknown>) => {
    if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
      Dialog.alert(title, String(squashAtomCommandFailure(result)));
    }
  };

  const offersOnce =
    config?.environment.capabilities.oneTimeSchedules === true ||
    draft.task?.schedule.type === "once";
  // Older servers have no webhook schedules; a webhook task keeps the option to stay one.
  const offersWebhook =
    config?.environment.capabilities.webhookSchedules === true ||
    draft.task?.schedule.type === "webhook";
  const offersThreadModes = config?.environment.capabilities.scheduledTaskThreadModes === true;
  const save = async () => {
    const current = readEditor();
    if (!current || current.environmentId !== environmentId) return;
    const draft = current.draft;
    if (
      !readEnvironmentScope(environmentId, AuthOrchestrationOperateScope) ||
      submissionPending.current ||
      saving ||
      dictationPending ||
      taskMissing ||
      environmentUnavailable ||
      automaticBaseRefLoading ||
      (draft.schedule.mode === "webhook" && webhookDenial !== undefined && draft.enabled)
    )
      return;
    if (draft.schedule.mode === "webhook" && !offersWebhook) {
      Dialog.alert(
        gt("Webhooks are not available here"),
        gt(
          "This environment runs an older {product}. Update it to run a task on a webhook, or pick another schedule.",
          { product: "Onicode" },
        ),
      );
      return;
    }
    if (draft.schedule.mode === "webhook") {
      const parsedFilters = parseWebhookFilterDrafts(draft.schedule.filters, m);
      if ("error" in parsedFilters) {
        Dialog.alert(gt("Invalid filter"), parsedFilters.error);
        return;
      }
      const problems = webhookScheduleProblems(draft.schedule);
      const problem = problems.replayKey
        ? gt("Duplicate request key must contain a valid request placeholder.")
        : problems.coalesceKey
          ? gt("Coalesce key must contain a valid request placeholder.")
          : problems.coalesceWindow
            ? gt("Coalesce window must be whole seconds from 10 to 3600.")
            : null;
      if (problem !== null) {
        Dialog.alert(gt("Invalid webhook settings"), problem);
        return;
      }
      // The task the save is checked against; a refused save brings the stored one.
      const stored = draft.task;
      if (
        draft.schedule.signatureEnabled &&
        (problems.signatureHeader ||
          (!draft.schedule.signatureSecret.trim() && stored?.webhook?.hasSecret !== true))
      ) {
        Dialog.alert(
          gt("Signing secret is required"),
          gt("Enter the signature header and secret."),
        );
        return;
      }
    }
    const schedule = scheduleFromDraft(draft.schedule);
    if (draft.schedule.mode === "once" && !offersOnce) {
      Dialog.alert(
        gt("Once is not available here"),
        gt(
          "This environment runs an older {product}. Update it to schedule a one-time task, or pick another schedule.",
          { product: "Onicode" },
        ),
      );
      return;
    }
    if (
      !draft.title.trim() ||
      !draft.prompt.trim() ||
      !draft.projectId ||
      !draft.modelSelection ||
      !schedule ||
      (draft.workspace === "existing_worktree" && !draft.checkoutPath.trim())
    ) {
      Dialog.alert(
        gt("Incomplete task"),
        gt("Add a name, prompt, project, model, valid schedule, and checkout path if needed."),
      );
      return;
    }
    if (!projects.some((project) => project.id === draft.projectId)) {
      Dialog.alert(gt("Project unavailable"), gt("Choose a project in this environment."));
      return;
    }
    const input: ScheduledTaskUpsertInput = {
      ...(draft.task ? { id: draft.task.id, requireExisting: true } : {}),
      title: draft.title.trim(),
      prompt: draft.prompt.trim(),
      projectId: draft.projectId,
      modelSelection: withImplicitFastModeSelection(draft.modelSelection, config?.providers ?? []),
      schedule,
      enabled: draft.enabled,
      threadId: draft.threadId,
      ...(draft.threadId === null && draft.task?.threadId != null ? { unbindThread: true } : {}),
      ...(offersThreadModes ? { threadMode: draft.threadMode } : {}),
      workspaceStrategy:
        draft.workspace === "root"
          ? { type: "root" }
          : draft.workspace === "existing_worktree"
            ? {
                type: "existing_worktree",
                worktreePath: normalizeProjectPathForDispatch(draft.checkoutPath),
              }
            : {
                type: "worktree",
                baseRef: worktreeBaseRef,
                startFromOrigin: draft.startFromOrigin,
              },
      // Tasks have no mode picker; a mode above the administrator's ceiling is refused.
      runtimeMode:
        clampManagedRuntimeMode(config?.managedPolicy, draft.runtimeMode) ?? draft.runtimeMode,
      interactionMode: draft.task?.interactionMode ?? "default",
      creationSource: draft.task?.creationSource ?? "mobile",
      ...(draft.task === null ? {} : { expected: scheduledTaskExpected(draft.task) }),
    };
    submissionPending.current = true;
    setSaving(true);
    const result = await upsert({ environmentId, input });
    setSaving(false);
    if (result._tag === "Failure") {
      submissionPending.current = false;
      // Another client's change refuses the save; keep the user's edits over what is stored now.
      const stored =
        refusedScheduledTask(squashAtomCommandFailure(result)) ??
        AsyncResult.getOrElse(registry.get(tasksAtom), () => null)?.tasks.find(
          (task) => task.id === draft.task?.id,
        );
      if (stored !== undefined) setDraft((latest) => rebaseScheduledTaskDraft(latest, stored));
      failure(gt("Could not save task"), result);
      return;
    }
    onSaved();
  };

  return (
    <View className="gap-5">
      <View
        className="gap-5"
        pointerEvents={saving ? "none" : "auto"}
        accessibilityElementsHidden={saving}
        importantForAccessibility={saving ? "no-hide-descendants" : "auto"}
      >
        {taskMissing ? (
          <Text className="px-1 text-base text-danger-foreground">
            <T>This task no longer exists.</T>
          </Text>
        ) : null}

        {environmentUnavailable ? (
          <Text className="px-1 text-base text-danger-foreground">
            <T>This environment is disconnected. Reconnect before saving.</T>
          </Text>
        ) : null}
        <SettingsSection>
          <SelectRow
            label={gt("Runs on")}
            value={environmentLabel}
            valueIcon={
              <EnvironmentMachineSymbol
                kind={resolveEnvironmentMachineKind(config)}
                size={18}
                tintColorClassName="accent-foreground-muted"
              />
            }
            actions={
              draft.task || saving || dictationPending
                ? []
                : availableTargets.map((target) => ({
                    id: target.environmentId,
                    title: target.label,
                    image:
                      ENVIRONMENT_MACHINE_SYMBOLS[
                        resolveEnvironmentMachineKind(target.serverConfig)
                      ],
                    state: target.environmentId === environmentId ? "on" : undefined,
                  }))
            }
            onSelect={(id) => {
              const target = availableTargets.find((entry) => entry.environmentId === id);
              if (target) onChangeEnvironment(target);
            }}
          />
        </SettingsSection>
        <SettingsSection title={gt("Task", { $context: "section heading for a scheduled task" })}>
          <FormField
            label={gt("Name")}
            disabled={saving}
            value={draft.title}
            placeholder={gt("Check for issues")}
            onChange={(title) => setDraft((current) => ({ ...current, title }))}
          />
          {promptField}
        </SettingsSection>

        <SettingsSection title={gt("Context", { $context: "section heading: project and model" })}>
          <SelectRow
            label={gt("Project")}
            value={
              projects.find((project) => project.id === draft.projectId)?.title ??
              (projects.length ? gt("Choose project") : gt("No projects available"))
            }
            actions={projects.map((project) => ({
              id: project.id,
              title: project.title,
              state: project.id === draft.projectId ? "on" : undefined,
            }))}
            onSelect={(id) => {
              const project = projects.find((item) => item.id === id);
              if (!project) return;
              setEnvironmentDraft((current) => {
                const keepsExplicitModel =
                  current.modelSelectionIsExplicit &&
                  (current.modelSelection === null ||
                    !config?.settings ||
                    resolveProjectProviderInstanceEnabled(
                      config.settings,
                      project.id,
                      current.modelSelection.instanceId,
                    ));
                return {
                  ...changeDraftProject(current, project.id),
                  modelSelection: keepsExplicitModel
                    ? current.modelSelection
                    : scheduledTaskDefaultModel(config, project),
                  modelSelectionIsExplicit: keepsExplicitModel,
                };
              });
            }}
          />
          <PickerRow
            label={gt("Model")}
            borderTop
            value={
              modelOptions.find(
                (option) =>
                  option.selection.instanceId === shownSelection?.instanceId &&
                  option.selection.model === shownSelection?.model,
              )?.label ??
              shownSelection?.model ??
              (modelOptions.length ? gt("Choose model") : gt("No models available"))
            }
            onPress={() => navigation.navigate("SettingsScheduledTaskModel")}
            // The saved selection only seeds new threads; bound runs ignore it.
            disabled={
              saving || dictationPending || environmentUnavailable || draft.threadId !== null
            }
          />
        </SettingsSection>

        <SettingsSection
          title={gt("Destination")}
          footer={
            draft.threadMode === "new_thread"
              ? gt("Each run starts a new thread.")
              : draft.threadMode === "continue"
                ? draft.threadId === null
                  ? gt(
                      "Runs continue one conversation. The first run starts the thread. Deleting it also deletes this task.",
                    )
                  : gt("Runs continue one conversation. Deleting it also deletes this task.")
                : draft.threadId === null
                  ? gt(
                      "Runs share one thread, each in a fresh session without earlier runs' context. The first run starts the thread. Deleting it also deletes this task.",
                    )
                  : gt(
                      "Runs share one thread, each in a fresh session without earlier runs' context. Deleting it also deletes this task.",
                    )
          }
        >
          {offersThreadModes ? (
            <SelectRow
              label={gt("Runs go to")}
              value={m(THREAD_MODE_LABELS[draft.threadMode])}
              actions={(Object.keys(THREAD_MODE_LABELS) as ScheduledTaskThreadMode[]).map(
                (mode) => ({
                  id: mode,
                  title: m(THREAD_MODE_LABELS[mode]),
                  state: draft.threadMode === mode ? "on" : undefined,
                }),
              )}
              onSelect={(id) => {
                if (id === "new_thread")
                  setDraft((current) => ({ ...unbindDraftFromThread(current), threadMode: id }));
                else if (id === "continue" || id === "fresh_session")
                  setDraft((current) => ({ ...current, threadMode: id }));
              }}
            />
          ) : null}
          {!offersThreadModes || draft.threadMode !== "new_thread" ? (
            <PickerRow
              label={gt("Thread")}
              borderTop={offersThreadModes}
              value={
                draft.threadId === null && draft.threadMode !== "new_thread"
                  ? gt("Started by the first run")
                  : destinationLabel
              }
              onPress={() => navigation.navigate("SettingsScheduledTaskThread")}
              disabled={!draft.projectId || saving || dictationPending || environmentUnavailable}
            />
          ) : null}
        </SettingsSection>

        <SettingsSection title={gt("Workspace")}>
          <SelectRow
            label={gt("Run in")}
            value={
              draft.workspace === "worktree"
                ? gt("New worktree")
                : draft.workspace === "root"
                  ? gt("Project checkout")
                  : gt("Specific checkout")
            }
            actions={[
              {
                id: "worktree",
                title: gt("New worktree"),
                state: draft.workspace === "worktree" ? "on" : undefined,
              },
              {
                id: "root",
                title: gt("Project checkout"),
                state: draft.workspace === "root" ? "on" : undefined,
              },
              {
                id: "existing_worktree",
                title: gt("Specific checkout"),
                state: draft.workspace === "existing_worktree" ? "on" : undefined,
              },
            ]}
            onSelect={(id) => {
              if (id === "worktree" || id === "root" || id === "existing_worktree")
                setDraft((current) => ({ ...current, workspace: id }));
            }}
          />
          {draft.workspace === "worktree" ? (
            <PickerRow
              label={gt("Base branch")}
              value={resolveNewTaskBranchLabel(
                {
                  branchName: automaticBaseRefLoading ? null : worktreeBaseRef,
                  startFromOrigin: draft.startFromOrigin,
                  workspaceMode: "worktree",
                },
                m,
              )}
              borderTop
              disabled={!draft.projectId || saving || dictationPending || environmentUnavailable}
              onPress={() => navigation.navigate("SettingsScheduledTaskBranch")}
            />
          ) : null}
          {draft.workspace === "existing_worktree" ? (
            <FormField
              label={gt("Checkout path")}
              disabled={saving}
              value={draft.checkoutPath}
              borderTop
              onChange={(checkoutPath) =>
                setEnvironmentDraft((current) => ({
                  ...current,
                  checkoutPath: resolvePastedPath(current.checkoutPath, checkoutPath),
                }))
              }
            />
          ) : null}
        </SettingsSection>

        <SettingsSection title={gt("Schedule", { $context: "noun, section heading" })}>
          <View className="px-4 py-3">
            <SegmentedControl
              variant="card"
              options={[
                { value: "fixed_time", label: gt("At a time") },
                // Older servers reject one-time schedules on save.
                ...(offersOnce || draft.schedule.mode === "once"
                  ? [
                      {
                        value: "once" as const,
                        label: gt("Once", { $context: "one-time schedule option" }),
                      },
                    ]
                  : []),
                { value: "interval", label: gt("Interval") },
                ...((offersWebhook && webhookDenial === undefined) ||
                draft.schedule.mode === "webhook"
                  ? [{ value: "webhook" as const, label: gt("On webhook") }]
                  : []),
              ]}
              selected={draft.schedule.mode}
              onSelect={(mode) => {
                setTimePickerOpen(false);
                setRunAtPicker(null);
                setDraft((current) => ({
                  ...current,
                  prompt:
                    mode === "webhook" && !current.prompt.trim()
                      ? DEFAULT_WEBHOOK_PROMPT
                      : current.prompt,
                  schedule: {
                    ...current.schedule,
                    mode,
                    runAt:
                      mode === "once" && !current.schedule.runAt
                        ? new Date(Math.ceil(Date.now() / 3_600_000) * 3_600_000).toISOString()
                        : current.schedule.runAt,
                  },
                }));
              }}
            />
          </View>
          {draft.schedule.mode === "fixed_time" ? (
            <>
              <Pressable
                accessibilityRole="button"
                accessibilityLabel={
                  runAtZone
                    ? gt("Time, {time} {zone}", {
                        time: timeOfDayLabel,
                        zone: runAtZone,
                      })
                    : gt("Time, {time}", { time: timeOfDayLabel })
                }
                onPress={() => setTimePickerOpen((open) => !open)}
                className="min-h-14 flex-row items-center gap-3 border-t border-border-subtle px-4 py-3 active:opacity-70"
              >
                <Text textRole="body" className="text-foreground">
                  {gt("Time", { $context: "time of day" })}
                </Text>
                <Text
                  textRole="footnote"
                  className="min-w-0 flex-1 text-right text-foreground-muted"
                >
                  {timeOfDayLabel}
                  {runAtZone ? ` ${runAtZone}` : ""}
                </Text>
                <SymbolView
                  name="chevron.right"
                  size={14}
                  tintColorClassName="accent-chevron"
                  type="monochrome"
                />
              </Pressable>
              {timePickerOpen ? (
                <DateTimePicker
                  value={timePickerValue(draft.schedule.timeOfDay)}
                  mode="time"
                  display={Platform.OS === "ios" ? "spinner" : "default"}
                  onDismiss={() => setTimePickerOpen(false)}
                  onValueChange={(_, selected) => {
                    if (Platform.OS === "android") setTimePickerOpen(false);
                    const timeOfDay = `${String(selected.getHours()).padStart(2, "0")}:${String(selected.getMinutes()).padStart(2, "0")}`;
                    setDraft((current) => ({
                      ...current,
                      schedule: { ...current.schedule, timeOfDay },
                    }));
                  }}
                />
              ) : null}
              <SelectRow
                label={gt("Repeat", { $context: "noun, which days a task repeats on" })}
                value={repeatLabel(draft.schedule.weekdays, m, locale)}
                borderTop
                actions={[
                  {
                    id: "every_day",
                    title: gt("Every day"),
                    state: draft.schedule.weekdays.length === 7 ? "on" : undefined,
                  },
                  {
                    id: "weekdays",
                    title: gt("Weekdays"),
                    state: isWeekdays(draft.schedule.weekdays) ? "on" : undefined,
                  },
                  ...DAYS.map((day) => ({
                    id: String(day.index),
                    title: weekdayLabel(day.index, locale),
                    attributes: { keepsMenuPresented: true },
                    state: draft.schedule.weekdays.includes(day.index)
                      ? ("on" as const)
                      : undefined,
                  })),
                ]}
                onSelect={(id) => {
                  setDraft((current) => {
                    const weekdays =
                      id === "every_day"
                        ? DAYS.map((day) => day.index)
                        : id === "weekdays"
                          ? [1, 2, 3, 4, 5]
                          : (() => {
                              const day = DAYS.find((item) => String(item.index) === id);
                              if (!day) return current.schedule.weekdays;
                              return current.schedule.weekdays.includes(day.index)
                                ? current.schedule.weekdays.filter((index) => index !== day.index)
                                : [...current.schedule.weekdays, day.index];
                            })();
                    return { ...current, schedule: { ...current.schedule, weekdays } };
                  });
                }}
              />
            </>
          ) : webhookBlocked ? (
            <Text className="border-t border-border-subtle px-4 py-3 text-sm text-foreground-muted">
              {gt("{reason} Switch the task to a timer, or turn it off, to save it.", {
                reason: webhookDenial,
              })}
            </Text>
          ) : draft.schedule.mode === "once" ? (
            (["date", "time"] as const).map((field) => {
              const runAt = new Date(draft.schedule.runAt);
              const label =
                field === "date" ? formatRunAtDate(runAt) : formatScheduledRunTime(runAt, is24Hour);
              return (
                <View key={field}>
                  <Pressable
                    accessibilityRole="button"
                    accessibilityLabel={
                      field === "date"
                        ? gt("Date, {date}", { date: label })
                        : gt("Time, {time}", { time: label })
                    }
                    onPress={() => setRunAtPicker((open) => (open === field ? null : field))}
                    className="min-h-14 flex-row items-center gap-3 border-t border-border-subtle px-4 py-3 active:opacity-70"
                  >
                    <Text textRole="body" className="text-foreground">
                      {field === "date" ? gt("Date") : gt("Time", { $context: "time of day" })}
                    </Text>
                    <Text
                      textRole="footnote"
                      className="min-w-0 flex-1 text-right text-foreground-muted"
                    >
                      {label}
                    </Text>
                    <SymbolView
                      name="chevron.right"
                      size={14}
                      tintColorClassName="accent-chevron"
                      type="monochrome"
                    />
                  </Pressable>
                  {runAtPicker === field ? (
                    <DateTimePicker
                      value={runAt}
                      mode={field}
                      display={Platform.OS === "ios" ? "spinner" : "default"}
                      onDismiss={() => setRunAtPicker(null)}
                      onValueChange={(_, selected) => {
                        // Android shows a dialog that closes on pick; iOS stays inline.
                        if (Platform.OS === "android") setRunAtPicker(null);
                        setDraft((current) => ({
                          ...current,
                          schedule: { ...current.schedule, runAt: selected.toISOString() },
                        }));
                      }}
                    />
                  ) : null}
                </View>
              );
            })
          ) : draft.schedule.mode === "webhook" ? (
            <WebhookScheduleDetails
              environmentId={environmentId}
              task={liveTask}
              disabled={!canOperate || saving || environmentUnavailable}
              onOpenDeliveries={(task) =>
                navigation.navigate("SettingsScheduledTaskDeliveries", {
                  environmentId: String(environmentId),
                  taskId: String(task.id),
                })
              }
            />
          ) : (
            <>
              <FormField
                label={gt("Minutes between runs")}
                value={draft.schedule.intervalMinutes}
                keyboardType="decimal-pad"
                disabled={saving}
                borderTop
                onChange={(intervalMinutes) =>
                  setDraft((current) => ({
                    ...current,
                    schedule: { ...current.schedule, intervalMinutes },
                  }))
                }
              />
              {!(parseIntervalMinutes(draft.schedule.intervalMinutes) >= 1) ? (
                <Text className="px-4 pb-3 text-sm text-danger-foreground">
                  <T>Intervals must be at least 1 minute. Update this interval before saving.</T>
                </Text>
              ) : draft.task?.schedule.type === "interval" &&
                draft.task.schedule.everyMs < 60_000 ? (
                <Text className="px-4 pb-3 text-sm text-foreground-muted">
                  <T>
                    This task previously ran more than once per minute. Saving requires an interval
                    of at least 1 minute.
                  </T>
                </Text>
              ) : null}
            </>
          )}
          <View className="min-h-14 flex-row items-center gap-3 border-t border-border-subtle px-4 py-3">
            <Text textRole="body" className="min-w-0 flex-1 text-foreground">
              {gt("Enabled", { $context: "switch label, whether the task runs" })}
            </Text>
            <ThemedSwitch
              accessibilityLabel={gt("Task enabled")}
              value={draft.enabled}
              onValueChange={(enabled) => setDraft((current) => ({ ...current, enabled }))}
            />
          </View>
        </SettingsSection>
        {draft.schedule.mode === "fixed_time" ? (
          <Text className="px-2 text-sm text-foreground-muted">
            <T>Time uses the environment's time zone, which may differ from your phone's.</T>
          </Text>
        ) : null}
        {draft.schedule.mode === "webhook" && !webhookBlocked ? (
          <WebhookSettingsSections
            draft={draft}
            hasStoredSecret={draft.task?.webhook?.hasSecret === true}
            disabled={saving}
            setDraft={setDraft}
          />
        ) : null}
      </View>
      <Button
        tone="primary"
        fullWidth
        loading={saving}
        disabled={
          !canOperate ||
          dictationPending ||
          taskMissing ||
          environmentUnavailable ||
          automaticBaseRefLoading ||
          // The server refuses an enabled webhook task under this policy.
          webhookBlocked
        }
        onPress={() => void save()}
        label={saving ? gt("Saving…") : draft.task ? gt("Save changes") : gt("Create task")}
      />
    </View>
  );
}

function WebhookScheduleDetails({
  environmentId,
  task,
  disabled,
  onOpenDeliveries,
}: {
  readonly environmentId: EnvironmentId;
  readonly task: ScheduledTask | null;
  readonly disabled: boolean;
  readonly onOpenDeliveries: (task: ScheduledTask) => void;
}) {
  const gt = useGT();
  const m = useMessages();
  const [rotating, setRotating] = useState(false);
  const rotate = useAtomCommand(serverEnvironment.rotateScheduledTaskWebhookToken, {
    label: "scheduled task rotate webhook token",
    reportFailure: false,
  });
  const preparedConnection = usePreparedConnection(environmentId);
  const httpBaseUrl =
    preparedConnection._tag === "Some" ? preparedConnection.value.httpBaseUrl : null;
  const webhook = task?.schedule.type === "webhook" ? task.webhook : undefined;
  const resolved = webhook ? webhookAddress(webhook, httpBaseUrl, m) : null;
  return (
    <>
      <View className="gap-2 border-t border-border-subtle px-4 py-3">
        <Text className="text-sm text-foreground-muted">
          {gt(
            "The prompt can use {body}, {headers}, {query}, {wholeBody} and {request}. The filled-in prompt is all the agent sees.",
            {
              body: "{{body.a.b}}",
              headers: "{{headers.name}}",
              query: "{{query.name}}",
              wholeBody: "{{body}}",
              request: "{{request}}",
            },
          )}
        </Text>
        {task === null || resolved === null ? (
          <Text className="text-sm text-foreground-muted">
            <T>Save the task to get its webhook URL.</T>
          </Text>
        ) : (
          <>
            <Pressable
              accessibilityRole="button"
              accessibilityLabel={gt("Copy webhook URL")}
              accessibilityHint={gt("Copies the URL to the clipboard")}
              accessibilityState={{ disabled: !resolved.copyable }}
              disabled={!resolved.copyable}
              onPress={() => void tryCopyTextWithHaptic(resolved.address)}
              className="gap-1 active:opacity-70"
            >
              <Text textRole="body" className="text-foreground">
                {resolved.copyable ? gt("Webhook URL") : gt("Webhook path")}
              </Text>
              <Text className="text-sm text-foreground-muted" numberOfLines={2} selectable>
                {resolved.address}
              </Text>
            </Pressable>
            {resolved.note !== null ? (
              <Text className="text-sm text-foreground-muted">{resolved.note}</Text>
            ) : null}
            <Button
              label={rotating ? gt("Rotating…") : gt("Rotate URL")}
              tone="danger"
              size="compact"
              disabled={disabled || rotating}
              loading={rotating}
              onPress={() =>
                Dialog.alert(gt("Rotate URL?"), gt("The current URL stops working immediately."), [
                  { text: gt("Cancel"), style: "cancel" },
                  {
                    text: gt("Rotate", { $context: "verb, replace the webhook URL" }),
                    style: "destructive",
                    onPress: () => {
                      setRotating(true);
                      void rotate({ environmentId, input: { id: task.id } }).then((result) => {
                        setRotating(false);
                        if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
                          Dialog.alert(
                            gt("Could not rotate URL"),
                            String(squashAtomCommandFailure(result)),
                          );
                        }
                      });
                    },
                  },
                ])
              }
            />
          </>
        )}
      </View>
      {task !== null && task.schedule.type === "webhook" ? (
        <PickerRow
          label={gt("Deliveries")}
          value={gt("Recent requests")}
          borderTop
          disabled={disabled}
          onPress={() => onOpenDeliveries(task)}
        />
      ) : null}
    </>
  );
}

function WebhookSettingsSections({
  draft,
  hasStoredSecret,
  disabled,
  setDraft,
}: {
  readonly draft: Draft;
  readonly hasStoredSecret: boolean;
  readonly disabled: boolean;
  readonly setDraft: (update: (current: Draft) => Draft) => void;
}) {
  const gt = useGT();
  const schedule = draft.schedule;
  const setSchedule = (patch: Partial<Draft["schedule"]>) =>
    setDraft((current) => ({ ...current, schedule: { ...current.schedule, ...patch } }));
  return (
    <>
      <SettingsSection
        title={gt("Filters", { $context: "section heading, webhook request filters" })}
        footer={
          schedule.filters.length > 0
            ? gt(
                "Runs only when every filter matches. Other requests are logged as filtered. Separate “is one of” values with commas; a value can't contain a comma.",
              )
            : gt("Without filters, every request runs the task.")
        }
      >
        <WebhookFilterRows
          filters={schedule.filters}
          disabled={disabled}
          onChange={(update) =>
            setDraft((current) => ({
              ...current,
              schedule: { ...current.schedule, filters: update(current.schedule.filters) },
            }))
          }
        />
      </SettingsSection>
      <SettingsSection
        title={gt("Duplicate requests")}
        footer={gt(
          "Requests with the same duplicate key run only once within 7 days. Coalescing runs the latest payload when the window from the first delivery closes. Empty keys run normally.",
        )}
      >
        <FormField
          label={gt("Duplicate request key")}
          value={schedule.replayKey}
          placeholder="{{headers.delivery-id}}"
          disabled={disabled}
          verbatim
          onChange={(replayKey) => setSchedule({ replayKey })}
        />
        <FormField
          label={gt("Coalesce key template")}
          value={schedule.coalesceKey}
          placeholder="{{body.id}}"
          disabled={disabled}
          verbatim
          borderTop
          onChange={(coalesceKey) => setSchedule({ coalesceKey })}
        />
        <FormField
          label={gt("Coalesce window (seconds, 10–3600)")}
          value={schedule.coalesceWindowSeconds}
          placeholder="120"
          keyboardType="number-pad"
          disabled={disabled}
          borderTop
          onChange={(coalesceWindowSeconds) => setSchedule({ coalesceWindowSeconds })}
        />
      </SettingsSection>
      <SettingsSection
        title={gt("Signature")}
        footer={
          schedule.signatureEnabled && hasStoredSecret
            ? gt(
                "Leave the secret blank to keep the saved one. Turn off the signature check to remove it.",
              )
            : gt("Reject requests without a valid HMAC-SHA256 signature of the body.")
        }
      >
        <View className="min-h-14 flex-row items-center gap-3 px-4 py-3">
          <Text textRole="body" className="min-w-0 flex-1 text-foreground">
            {gt("Require signature")}
          </Text>
          <ThemedSwitch
            accessibilityLabel={gt("Require signature")}
            value={schedule.signatureEnabled}
            disabled={disabled}
            onValueChange={(signatureEnabled) => setSchedule({ signatureEnabled })}
          />
        </View>
        {schedule.signatureEnabled ? (
          <>
            <FormField
              label={gt("Header", { $context: "HTTP header that carries the signature" })}
              value={schedule.signatureHeader}
              disabled={disabled}
              verbatim
              borderTop
              onChange={(signatureHeader) => setSchedule({ signatureHeader })}
            />
            <FormField
              label={gt("Prefix", { $context: "text before the signature digest" })}
              value={schedule.signaturePrefix}
              placeholder={gt("None")}
              disabled={disabled}
              verbatim
              borderTop
              onChange={(signaturePrefix) => setSchedule({ signaturePrefix })}
            />
            <SelectRow
              label={gt("Encoding", { $context: "how the signature digest is written" })}
              value={schedule.signatureEncoding}
              borderTop
              actions={(["hex", "base64"] as const).map((encoding) => ({
                id: encoding,
                title: encoding,
                state: schedule.signatureEncoding === encoding ? ("on" as const) : undefined,
              }))}
              onSelect={(id) => {
                if (id === "hex" || id === "base64") setSchedule({ signatureEncoding: id });
              }}
            />
            <FormField
              label={gt("Secret", { $context: "shared secret that signs webhook requests" })}
              value={schedule.signatureSecret}
              placeholder={hasStoredSecret ? gt("Unchanged") : gt("Shared secret")}
              disabled={disabled}
              secure
              borderTop
              onChange={(signatureSecret) => setSchedule({ signatureSecret })}
            />
          </>
        ) : null}
      </SettingsSection>
    </>
  );
}

function EnvironmentTasks({
  environment,
  now,
  projectIds,
  onEdit,
}: {
  readonly environment: SettingsTarget;
  readonly now: number;
  readonly projectIds: readonly ProjectId[] | null;
  readonly onEdit: (task: ScheduledTask) => void;
}) {
  const gt = useGT();
  const m = useMessages();
  const locale = useLocale();
  const is24Hour = useSystem24HourClock();
  const environmentId = environment.environmentId;
  const tasks = useEnvironmentQuery(
    serverEnvironment.scheduledTasksLive({ environmentId, input: {} }),
  );
  const scheduleLabel = (task: ScheduledTask) =>
    describeSchedule(task, tasks.data?.timeZone, m, locale, is24Hour);
  const visibleTasks = tasks.data?.tasks.filter(
    (task) => projectIds === null || projectIds.includes(task.projectId),
  );
  const canOperate = useAtomValue(
    serverEnvironment.upsertScheduledTask.permissionAtom(environmentId),
  );
  const setEnabled = useAtomCommand(serverEnvironment.setScheduledTaskEnabled, {
    label: "scheduled task enabled",
    reportFailure: false,
  });
  const runNow = useAtomCommand(serverEnvironment.runScheduledTaskNow, {
    label: "scheduled task run",
    reportFailure: false,
  });
  const remove = useAtomCommand(serverEnvironment.deleteScheduledTask, {
    label: "scheduled task delete",
    reportFailure: false,
  });
  const selectDetail = useSelectWorkspaceDetail();
  const navigation = useNavigation<NativeStackNavigationProp<ScheduledTaskRoutes>>();
  const failure = (title: string, result: AtomCommandResult<unknown, unknown>) => {
    if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
      Dialog.alert(title, String(squashAtomCommandFailure(result)));
    }
  };

  const act = async (task: ScheduledTask, action: "run" | "toggle" | "delete") => {
    if (!readEnvironmentScope(environmentId, AuthOrchestrationOperateScope)) return;
    const result =
      action === "run"
        ? await runNow({ environmentId, input: { id: task.id } })
        : action === "toggle"
          ? await setEnabled({ environmentId, input: { id: task.id, enabled: !task.enabled } })
          : await remove({ environmentId, input: { id: task.id } });
    failure(
      action === "run"
        ? gt("Could not run task")
        : action === "toggle"
          ? gt("Could not update task")
          : gt("Could not delete task"),
      result,
    );
  };

  return (
    <SettingsSection
      title={environment.label}
      titleIcon={
        <EnvironmentMachineSymbol
          kind={resolveEnvironmentMachineKind(environment.serverConfig)}
          size={16}
          tintColorClassName={
            Platform.OS === "android" ? "accent-primary" : "accent-foreground-muted"
          }
        />
      }
    >
      {tasks.error ? (
        <Text className="p-4 text-base text-danger-foreground">{tasks.error}</Text>
      ) : !tasks.data ? (
        <Text className="p-4 text-base text-foreground-muted">
          <T>Loading tasks…</T>
        </Text>
      ) : visibleTasks?.length === 0 ? (
        <Text className="p-4 text-base text-foreground-muted">
          {projectIds === null ? gt("No scheduled tasks yet.") : gt("No tasks in this project.")}
        </Text>
      ) : (
        visibleTasks?.map((task, index) => (
          <View
            key={task.id}
            className={
              index === 0
                ? "flex-row items-start gap-1 px-4 py-3"
                : "flex-row items-start gap-1 border-t border-border-subtle px-4 py-3"
            }
          >
            <Pressable
              accessibilityRole="button"
              accessibilityLabel={gt("Edit {title}", { title: task.title })}
              onPress={() => {
                onEdit(task);
              }}
              className="min-w-0 flex-1 gap-1 active:opacity-70"
            >
              <Text textRole="body" className="text-foreground" numberOfLines={1}>
                {task.title}
              </Text>
              <Text className="text-sm text-foreground-muted" numberOfLines={2}>
                {!task.enabled
                  ? gt("{schedule} · Paused", {
                      schedule: scheduleLabel(task),
                    })
                  : task.nextRunAt
                    ? gt("{schedule} · {nextRun}", {
                        schedule: scheduleLabel(task),
                        nextRun: formatNextScheduledTaskRun(task.nextRunAt, now, m, is24Hour),
                      })
                    : scheduleLabel(task)}
              </Text>
              {task.blockedByPolicy ? (
                <Text className="text-sm text-foreground-muted" numberOfLines={2}>
                  {task.blockedByPolicy}
                </Text>
              ) : null}
              {task.lastRunError ? (
                <Text className="text-sm text-danger-foreground" numberOfLines={2}>
                  <T>
                    Last run failed: <Var>{task.lastRunError}</Var>
                  </T>
                </Text>
              ) : null}
            </Pressable>
            <Menu
              actions={[
                { id: "edit", title: gt("Edit") },
                ...(task.threadId !== null && scheduledTaskThreadMode(task) !== "new_thread"
                  ? [{ id: "thread", title: gt("Open thread") }]
                  : []),
                {
                  id: "toggle",
                  title: task.enabled ? gt("Pause") : gt("Resume"),
                  attributes: { disabled: !canOperate },
                },
                // A webhook task runs on a request; its deliveries show what came in.
                ...(task.schedule.type === "webhook"
                  ? [
                      {
                        id: "deliveries",
                        title: gt("Deliveries"),
                        attributes: { disabled: !canOperate },
                      },
                    ]
                  : [{ id: "run", title: gt("Run now"), attributes: { disabled: !canOperate } }]),
                {
                  id: "delete",
                  title: gt("Delete"),
                  attributes: { destructive: true, disabled: !canOperate },
                },
              ]}
              onPressAction={({ nativeEvent }) => {
                const action = nativeEvent.event;
                if (action === "edit") {
                  onEdit(task);
                } else if (action === "deliveries") {
                  navigation.navigate("SettingsScheduledTaskDeliveries", {
                    environmentId: String(environmentId),
                    taskId: String(task.id),
                  });
                } else if (action === "thread" && task.threadId !== null) {
                  selectDetail({
                    name: "Thread",
                    params: {
                      environmentId: String(environmentId),
                      threadId: String(task.threadId),
                    },
                  });
                } else if (action === "delete") {
                  Dialog.alert(gt("Delete task?"), task.title, [
                    { text: gt("Cancel"), style: "cancel" },
                    {
                      text: gt("Delete"),
                      style: "destructive",
                      onPress: () => void act(task, "delete"),
                    },
                  ]);
                } else if (action === "toggle" || action === "run") {
                  void act(task, action);
                }
              }}
            >
              <Pressable
                accessibilityRole="button"
                accessibilityLabel={gt("Actions for {title}", { title: task.title })}
                className="h-11 w-11 items-center justify-center"
              >
                <SymbolView
                  name="ellipsis"
                  size={18}
                  tintColorClassName="accent-icon"
                  type="monochrome"
                />
              </Pressable>
            </Menu>
          </View>
        ))
      )}
    </SettingsSection>
  );
}
