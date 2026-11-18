import { Slider } from "@base-ui/react/slider";
import {
  buildTraitsTriggerDisplay,
  getDescriptorStringValue,
  isReasoningControl,
  selectableProviderOptionChoices,
} from "@onicode/client-runtime/model-traits";
import type { ProviderOptionDescriptor, RuntimeMode } from "@onicode/contracts";
import type { EffortDisplay } from "@onicode/contracts/settings";
import { getProviderOptionCurrentLabel } from "@onicode/shared/model";
import { WorkflowIcon } from "lucide-react";
import { type CSSProperties, type KeyboardEvent, type ReactNode, useId, useState } from "react";
import { msg, useGT, useMessages } from "gt-react";

import { m, type MessageResolver } from "~/i18n/messages";
import { cn } from "~/lib/utils";
import { Switch } from "../ui/switch";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import type { ComposerControlSize } from "./ComposerControl";
import type { RuntimeModeOption } from "./ComposerFooterControls";
import { EffortGlyph, FastBolt, resolveEffortGlyph } from "./EffortGlyph";
import { EffortSliderField, effortHue, effortTone } from "./EffortSliderField";
import {
  getTraitsSectionVisibility,
  type TraitsSelectionInput,
  ULTRATHINK_IN_BODY_NOTE,
  useTraitsSelection,
} from "./TraitsPicker";

type SelectDescriptor = Extract<ProviderOptionDescriptor, { type: "select" }>;
type BooleanDescriptor = Extract<ProviderOptionDescriptor, { type: "boolean" }>;

type ColumnControl =
  | { kind: "select"; descriptor: SelectDescriptor }
  | { kind: "boolean"; descriptor: BooleanDescriptor };

/**
 * The section lists the model's reasoning levels under the header that names it. Every other
 * option, such as Fast mode, Ultracode or an agent's session mode, sits below as one row each,
 * with its choices in the row: the picker is one popup, with no menu behind a row.
 */
function columnControls(
  selects: ReadonlyArray<SelectDescriptor>,
  toggles: ReadonlyArray<BooleanDescriptor>,
) {
  const controls: ReadonlyArray<ColumnControl> = [
    ...selects.map((descriptor) => ({ kind: "select" as const, descriptor })),
    ...toggles.map((descriptor) => ({ kind: "boolean" as const, descriptor })),
  ];
  const effort = controls.find((control) => isReasoningControl(control.descriptor)) ?? null;
  return { effort, others: controls.filter((control) => control !== effort) };
}

/**
 * What the model trigger shows after the model name: the effort word (or bars, per the
 * Appearance setting), a bolt for Fast, a workflow icon for Ultracode, and other traits such as a
 * context window.
 */
export function describeComposerEffort(
  traits: TraitsSelectionInput | null,
  effortDisplay: EffortDisplay,
  size: ComposerControlSize,
  resolve: MessageResolver = m,
): { suffix: ReactNode; detail: string | null } {
  if (!traits) return { suffix: null, detail: null };
  const visibility = getTraitsSectionVisibility({ ...traits, resolve });
  if (!visibility.hasAnyControls) return { suffix: null, detail: null };
  const modelSelection =
    traits.instanceId && traits.model
      ? { instanceId: traits.instanceId, model: traits.model, options: traits.modelOptions ?? [] }
      : null;
  const primary = visibility.primarySelectDescriptor;
  const display = buildTraitsTriggerDisplay(
    {
      descriptors: visibility.descriptors,
      primarySelectDescriptorId: primary?.id ?? null,
      ultrathinkPromptControlled: visibility.ultrathinkPromptControlled,
      modelSelection,
      reportedModelSelection: traits.reportedModelSelection,
    },
    resolve,
  );
  // The same reasoning control the section lists, wherever the agent put it among its options.
  const reasoning =
    columnControls(visibility.selectDescriptors, visibility.booleanDescriptors).effort
      ?.descriptor ?? null;
  const ultrathink = visibility.ultrathinkPromptControlled && reasoning === primary;
  const glyph =
    reasoning?.type === "select"
      ? resolveEffortGlyph(
          reasoning,
          ultrathink
            ? "ultrathink"
            : getDescriptorStringValue(reasoning, modelSelection, traits.reportedModelSelection),
        )
      : null;
  // Workflow levels such as Ultracode always show their name; the setting names every level.
  const word = glyph
    ? (glyph.special ?? (effortDisplay === "label" ? glyph.label : null))
    : !reasoning
      ? null
      : reasoning.type === "boolean"
        ? reasoning.currentValue === true
          ? resolve(msg("{option} On", { option: reasoning.label }))
          : resolve(msg("{option} Off", { option: reasoning.label }))
        : ultrathink
          ? "Ultrathink"
          : (getProviderOptionCurrentLabel(
              reasoning,
              modelSelection,
              traits.reportedModelSelection,
            ) ?? (reasoning.id === "thinking" ? reasoning.label : null));
  const fast = display.speedLabel === "Fast" || display.speedLabel === "Ultrafast";
  // Agent options such as modes live in the picker; the trigger adds only the context window.
  const contextWindow =
    visibility.contextWindowDescriptor &&
    getProviderOptionCurrentLabel(
      visibility.contextWindowDescriptor,
      modelSelection,
      traits.reportedModelSelection,
    );
  if (!word && !glyph && !fast && !display.ultracode && !contextWindow)
    return { suffix: null, detail: display.label };
  return {
    detail: display.label,
    suffix: (
      <span
        className={cn(
          "flex shrink-0 items-center gap-1.5 text-muted-foreground group-hover/model-trigger:text-foreground group-data-pressed/model-trigger:text-foreground group-data-popup-open/model-trigger:text-foreground",
          // A word space after the model name: the regular trigger's gap is wider than one.
          size === "sm" && "-ms-0.5",
        )}
      >
        {word ? (
          <span data-composer-control-label className="max-w-24 truncate">
            {word}
          </span>
        ) : glyph ? (
          <EffortGlyph count={glyph.count} filled={glyph.filled} size={size} />
        ) : null}
        {fast ? <FastBolt ultra={display.speedLabel === "Ultrafast"} /> : null}
        {display.ultracode ? (
          <WorkflowIcon aria-hidden="true" data-composer-control-icon className="size-3" />
        ) : null}
        {contextWindow ? (
          <span
            // A label, so icon-only footers drop it; compacted rows drop it before the model.
            data-composer-control-label
            data-composer-secondary-trait
            className="max-w-24 truncate text-xs"
          >
            {contextWindow}
          </span>
        ) : null}
      </span>
    ),
  };
}

const OPTION_ROW =
  "flex h-7 w-full shrink-0 cursor-pointer items-center gap-2 rounded-md px-2 text-left text-sm text-foreground outline-none hover:bg-row-highlight focus-visible:bg-row-highlight disabled:cursor-not-allowed disabled:opacity-50";
const COLUMN_LABEL = "px-2 pt-1.5 pb-1 font-medium text-muted-foreground text-xs";

/** The track a group of segments sits in; the picker's footer uses the same one. */
const SEGMENT_TRACK = "flex rounded-lg bg-input/40 p-0.5";
const SEGMENT =
  "flex h-6 cursor-pointer items-center justify-center gap-1 rounded-md px-2 text-muted-foreground text-xs outline-none hover:text-foreground data-checked:bg-input/72 data-checked:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset disabled:cursor-not-allowed disabled:opacity-64";

function LevelSegment(props: {
  label: string;
  description?: string | undefined;
  selected: boolean;
  /** The control's one tab stop: the selected choice, else the first when none shows as selected. */
  tabStop: boolean;
  /** A choice of an option besides reasoning; focus after a model pick passes it by. */
  option?: boolean;
  isDefault?: boolean | undefined;
  disabled?: boolean;
  onPick: () => void;
}) {
  const gt = useGT();
  const segmentProps = {
    type: "button",
    role: "radio",
    "aria-checked": props.selected,
    "data-checked": props.selected || undefined,
    disabled: props.disabled,
    "data-effort-row": true,
    // Focus lands on a level after a model pick.
    "data-effort-level": props.option ? undefined : true,
    // Left and Right move between the choices.
    tabIndex: props.tabStop ? 0 : -1,
    // Levels share the row evenly until their names need more, then wrap.
    className: cn(SEGMENT, "min-w-max flex-1"),
    onClick: props.onPick,
  } as const;
  // A segment has no room for a "Default" tag beside its name, so the hint carries it.
  const namesDefault = props.isDefault === true && props.label !== "Default";
  const hint = [namesDefault ? "Default" : null, props.description].filter(Boolean).join(" · ");
  const content = (
    <>
      {props.label}
      {namesDefault ? (
        <span className="sr-only">, {gt("Default", { $context: "marks the default option" })}</span>
      ) : null}
    </>
  );
  if (!hint) return <button {...segmentProps}>{content}</button>;
  return (
    <Tooltip>
      <TooltipTrigger render={<button {...segmentProps} />}>{content}</TooltipTrigger>
      <TooltipPopup side="top" className="max-w-56">
        {hint}
      </TooltipPopup>
    </Tooltip>
  );
}

/**
 * Left and Right walk the stops of one control: the levels, or one option's choices. Down goes on
 * to the next control and Up comes back; Up from the first is left for the popup, which returns
 * to the list. A slider and a switch answer Left and Right themselves.
 */
function moveBetweenRows(event: KeyboardEvent<HTMLElement>) {
  const { key } = event;
  if (key !== "ArrowDown" && key !== "ArrowUp" && key !== "ArrowLeft" && key !== "ArrowRight")
    return;
  const rows = Array.from(
    event.currentTarget.querySelectorAll<HTMLElement>("[data-effort-row]:not(:disabled)"),
  );
  const active = rows.find((row) => row === document.activeElement);
  if (!active) return;
  const controlOf = (row: HTMLElement) => row.closest('[role="radiogroup"]') ?? row;
  const stopsOf = (control: Element) => rows.filter((row) => controlOf(row) === control);
  const controls = [...new Set(rows.map(controlOf))];
  const control = controlOf(active);
  let next = active;
  if (key === "ArrowLeft" || key === "ArrowRight") {
    const stops = stopsOf(control);
    if (stops.length < 2) return;
    const index = stops.indexOf(active) + (key === "ArrowRight" ? 1 : -1);
    next = stops[Math.min(stops.length - 1, Math.max(0, index))] ?? active;
  } else {
    const target = controls[controls.indexOf(control) + (key === "ArrowDown" ? 1 : -1)];
    if (!target && key === "ArrowUp") return;
    if (target) {
      const stops = stopsOf(target);
      next = stops.find((row) => row.getAttribute("aria-checked") === "true") ?? stops[0] ?? active;
    }
  }
  event.preventDefault();
  event.stopPropagation();
  next.focus();
  next.scrollIntoView({ block: "nearest" });
}

type LevelChoice = ReturnType<typeof selectableProviderOptionChoices>[number];

/** Fewer levels than this read better as segments: a two-stop slider is a switch. */
const SLIDER_MIN_LEVELS = 3;

/** The section's key handling and the picker's focus find the levels by these marks. */
function markEffortLevel(input: HTMLInputElement | null) {
  if (!input) return;
  input.dataset.effortRow = "";
  input.dataset.effortLevel = "";
}

function levelHint(choice: LevelChoice): string {
  const name =
    choice.isDefault && choice.label !== "Default" ? `${choice.label} · Default` : choice.label;
  return choice.description ? `${name}. ${choice.description}` : name;
}

/** The reasoning levels as one slider: a stop per level, filled from the fast end to the thumb. */
function EffortSlider(props: {
  label: string;
  choices: ReadonlyArray<LevelChoice>;
  /** The picked level, or -1 when the prompt asks for one the slider does not offer. */
  index: number;
  /** The level under the thumb, which runs ahead of the pick while it is dragged. */
  shownIndex: number;
  /** The shown level's place on the hue range. */
  tone: number;
  disabled: boolean;
  onDrag: (index: number | null) => void;
  onPick: (index: number) => void;
}) {
  const last = props.choices.length - 1;
  const shown = Math.max(0, props.shownIndex);
  return (
    <Slider.Root
      value={shown}
      min={0}
      max={last}
      step={1}
      disabled={props.disabled}
      onValueChange={props.onDrag}
      onValueCommitted={(index) => {
        props.onDrag(null);
        if (index !== props.index) props.onPick(index);
      }}
      className="group/effort flex"
    >
      <Slider.Control className="effort-slider-track relative flex h-7 min-w-0 flex-1 cursor-pointer touch-none items-center overflow-hidden rounded-lg select-none data-disabled:cursor-not-allowed data-disabled:opacity-64">
        {props.shownIndex < 0 ? null : (
          <EffortSliderField level={shown} top={last} tone={props.tone} />
        )}
        {/* Inset by half the thumb and its gap, so the thumb stays inside the fill at both ends. */}
        <Slider.Track className="relative mx-2.5 h-6 flex-1">
          <span aria-hidden="true" className="pointer-events-none absolute inset-0">
            {props.choices.map((choice, index) => (
              <Tooltip key={choice.id}>
                <TooltipTrigger
                  render={
                    <span
                      className="pointer-events-auto absolute top-1/2 flex size-4 -translate-1/2 items-center justify-center group-data-dragging/effort:pointer-events-none"
                      style={{ left: `${(index / last) * 100}%` }}
                    />
                  }
                >
                  {/* The fill covers the stops it has passed. */}
                  <span
                    className={cn(
                      "h-2 w-px bg-foreground/20 transition-opacity duration-150",
                      index <= shown && "opacity-0",
                    )}
                  />
                </TooltipTrigger>
                <TooltipPopup side="top" className="max-w-56">
                  {levelHint(choice)}
                </TooltipPopup>
              </Tooltip>
            ))}
          </span>
          <Slider.Thumb
            getAriaLabel={() => props.label}
            getAriaValueText={(_formatted, index) => props.choices[index]?.label ?? ""}
            inputRef={markEffortLevel}
            // Up and Down belong to the section: they leave the slider for the list and the rows.
            onKeyDown={(event) => {
              if (event.key === "ArrowUp" || event.key === "ArrowDown") event.preventDefault();
            }}
            // The same glide as the fill's width, so the thumb stays on the fill's end.
            className={cn(
              "effort-slider-thumb z-10 h-6 w-4 rounded-md transition-[left,scale] duration-200 ease-out group-data-dragging/effort:scale-95 has-focus-visible:outline-2 has-focus-visible:-outline-offset-2 has-focus-visible:outline-ring motion-reduce:transition-none",
              props.shownIndex < 0 && "invisible",
            )}
          />
        </Slider.Track>
      </Slider.Control>
    </Slider.Root>
  );
}

/** The level's name beside the section's label. It rolls in from the side the level moved from. */
function EffortLevelName(props: {
  name: string;
  index: number;
  /** The level's place on the hue range, which picks its color. */
  tone: number;
  isDefault: boolean;
}) {
  const gt = useGT();
  const [seen, setSeen] = useState({ index: props.index, from: 0 });
  if (seen.index !== props.index) {
    setSeen({ index: props.index, from: props.index > seen.index ? 1 : -1 });
  }
  return (
    <>
      {props.isDefault && props.name !== "Default" ? (
        <span className="text-muted-foreground">
          {gt("Default", { $context: "marks the default option" })}
        </span>
      ) : null}
      <span
        key={props.name}
        className={cn(
          "effort-level-ink font-medium",
          seen.from !== 0 && "motion-safe:animate-effort-level-roll",
        )}
        style={
          {
            "--effort-level-from": `${seen.from * 0.5}em`,
            "--effort-level-hue": effortHue(props.tone),
          } as CSSProperties
        }
      >
        {props.name}
      </span>
    </>
  );
}

/** The section's frame: its label with the picked level across from it, then the rows. */
function EffortFrame(props: { label: string; level?: ReactNode; children: ReactNode }) {
  return (
    <>
      <div className="flex shrink-0 items-baseline gap-1.5 px-3 pt-2 pb-1.5 text-xs">
        <span className="me-auto font-medium text-muted-foreground">{props.label}</span>
        {props.level}
      </div>
      <div
        className="flex min-h-0 flex-col overflow-y-auto overscroll-contain px-1 pb-1"
        // Capture, so Up and Down move between controls before a slider takes them.
        onKeyDownCapture={moveBetweenRows}
      >
        {props.children}
      </div>
    </>
  );
}

function EffortSection(props: { traits: TraitsSelectionInput }) {
  const gt = useGT();
  const messages = useMessages();
  const traits = useTraitsSelection(props.traits);
  const { effort, others } = columnControls(traits.selectDescriptors, traits.booleanDescriptors);
  // The level under the thumb while it is dragged. The pick lands when it is let go.
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const label =
    effort?.descriptor.label ??
    (others.length > 0
      ? gt("Options", { $context: "heading for model options" })
      : gt("Effort", { $context: "heading for reasoning effort levels" }));
  if (traits.modelIsUnavailable) {
    return (
      <EffortFrame label={label}>
        {traits.descriptors.map((descriptor) => {
          const value = traits.currentLabel(descriptor);
          return value ? (
            <div key={descriptor.id}>
              <div className={COLUMN_LABEL}>{descriptor.label}</div>
              <div className="px-2 pb-1.5 text-muted-foreground text-xs">{value}</div>
            </div>
          ) : null;
        })}
      </EffortFrame>
    );
  }
  if (!effort && others.length === 0) return null;
  // Ultrathink in the prompt locks the levels; the first option then holds the tab stop.
  const locked =
    effort?.descriptor.id === traits.primarySelectDescriptor?.id && traits.ultrathinkInBodyText;
  const slider =
    effort?.kind === "select"
      ? {
          descriptor: effort.descriptor,
          levels: selectableProviderOptionChoices(effort.descriptor),
        }
      : null;
  const slides = slider !== null && slider.levels.length >= SLIDER_MIN_LEVELS;
  // An "Ultrathink:" prefix selects a level the section does not offer, which leaves -1 here.
  const index = slides
    ? slider.levels.findIndex((level) => level.id === traits.selectValue(slider.descriptor))
    : -1;
  const shownIndex = dragIndex ?? index;
  const shownLevel = slides ? slider.levels[shownIndex] : undefined;
  const tone = slides
    ? effortTone(
        shownIndex,
        slider.levels.length - 1,
        slider.levels.findIndex((level) => level.isDefault === true),
      )
    : 0.5;
  return (
    <EffortFrame
      label={label}
      level={
        shownLevel ? (
          <EffortLevelName
            name={shownLevel.label}
            index={shownIndex}
            tone={tone}
            isDefault={shownLevel.isDefault === true}
          />
        ) : null
      }
    >
      {slides ? (
        <>
          {locked ? (
            <div className="px-2 pb-1.5 text-muted-foreground text-xs">
              {messages(ULTRATHINK_IN_BODY_NOTE)}
            </div>
          ) : null}
          <div className="px-2">
            <EffortSlider
              label={slider.descriptor.label}
              choices={slider.levels}
              index={index}
              shownIndex={shownIndex}
              tone={tone}
              disabled={locked}
              onDrag={setDragIndex}
              onPick={(next) => {
                const level = slider.levels[next];
                if (level) traits.changeSelect(slider.descriptor, level.id);
              }}
            />
          </div>
        </>
      ) : effort ? (
        <EffortRows control={effort} traits={traits} locked={locked} />
      ) : null}
      {others.length > 0 ? (
        <div className={cn("flex flex-col", effort && "mt-1.5")}>
          {others.map((control, index) =>
            control.kind === "boolean" ? (
              <OptionSwitchRow
                key={control.descriptor.id}
                descriptor={control.descriptor}
                traits={traits}
                tabStop={(!effort || locked) && index === 0}
              />
            ) : (
              <OptionChoicesRow
                key={control.descriptor.id}
                descriptor={control.descriptor}
                traits={traits}
                tabStop={(!effort || locked) && index === 0}
              />
            ),
          )}
        </div>
      ) : null}
    </EffortFrame>
  );
}

type TraitsSelection = ReturnType<typeof useTraitsSelection>;

function EffortRows(props: { control: ColumnControl; traits: TraitsSelection; locked: boolean }) {
  const gt = useGT();
  const messages = useMessages();
  const { control, traits } = props;
  const { descriptor } = control;
  if (control.kind === "boolean") {
    return (
      <div role="radiogroup" aria-label={descriptor.label} className={SEGMENT_TRACK}>
        {([true, false] as const).map((value) => (
          <LevelSegment
            key={String(value)}
            label={
              value
                ? gt("On", { $context: "option state" })
                : gt("Off", { $context: "option state" })
            }
            selected={(control.descriptor.currentValue === true) === value}
            tabStop={(control.descriptor.currentValue === true) === value}
            onPick={() => traits.changeBoolean(descriptor.id, value)}
          />
        ))}
      </div>
    );
  }
  const { locked } = props;
  const value = traits.selectValue(control.descriptor);
  const choices = selectableProviderOptionChoices(control.descriptor);
  // An "Ultrathink:" prefix selects a level the section does not offer.
  const shown = choices.some((option) => option.id === value);
  return (
    <>
      {locked ? (
        <div className="px-1 pb-1.5 text-muted-foreground text-xs">
          {messages(ULTRATHINK_IN_BODY_NOTE)}
        </div>
      ) : null}
      <div
        role="radiogroup"
        aria-label={descriptor.label}
        className={cn(SEGMENT_TRACK, "flex-wrap")}
      >
        {choices.map((option, index) => (
          <LevelSegment
            key={option.id}
            label={option.label}
            selected={option.id === value}
            tabStop={shown ? option.id === value : index === 0}
            description={option.description}
            isDefault={option.isDefault}
            disabled={locked}
            onPick={() => traits.changeSelect(control.descriptor, option.id)}
          />
        ))}
      </div>
    </>
  );
}

/**
 * One select besides reasoning, such as an agent's session mode: its name, then every choice as a
 * segment. The choices sit beside the name while they fit, and take the line under it when not.
 */
function OptionChoicesRow(props: {
  descriptor: SelectDescriptor;
  traits: TraitsSelection;
  /** Holds the section's tab stop when the model has no levels. */
  tabStop: boolean;
}) {
  const { descriptor, traits } = props;
  const choices = selectableProviderOptionChoices(descriptor);
  const value = traits.selectValue(descriptor);
  const shown = choices.some((choice) => choice.id === value);
  return (
    <div className="flex min-h-7 flex-wrap items-center gap-x-3 gap-y-1 px-2 py-0.5 text-sm">
      {/* The name takes a shared line's spare room, so the choices stretch only on their own line. */}
      <span className="grow-999 whitespace-nowrap">{descriptor.label}</span>
      {choices.length < 2 ? (
        // Nothing to choose between, so the row only reports the value.
        <span className="truncate text-muted-foreground text-xs">{choices[0]?.label}</span>
      ) : (
        <div
          role="radiogroup"
          aria-label={descriptor.label}
          className={cn(SEGMENT_TRACK, "max-w-full grow flex-wrap")}
        >
          {choices.map((choice, index) => (
            <LevelSegment
              key={choice.id}
              option
              label={choice.label}
              description={choice.description}
              isDefault={choice.isDefault}
              selected={choice.id === value}
              tabStop={props.tabStop && (shown ? choice.id === value : index === 0)}
              onPick={() => {
                if (choice.id !== value) traits.changeSelect(descriptor, choice.id);
              }}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/** A two-state option toggles in place. */
function OptionSwitchRow(props: {
  descriptor: BooleanDescriptor;
  traits: TraitsSelection;
  tabStop: boolean;
}) {
  const { descriptor, traits } = props;
  const checked = descriptor.currentValue === true;
  return (
    <label className={cn(OPTION_ROW, "has-focus-visible:bg-row-highlight")}>
      <span className="min-w-0 flex-1 truncate">{descriptor.label}</span>
      <Switch
        size="sm"
        checked={checked}
        onCheckedChange={(next) => traits.changeBoolean(descriptor.id, next)}
        aria-label={descriptor.label}
        data-effort-row
        tabIndex={props.tabStop ? 0 : -1}
      />
    </label>
  );
}

/** The model picker's effort section: the selected model's levels and its other options. */
export function ComposerEffortColumn(props: { traits: TraitsSelectionInput | null }) {
  return props.traits ? <EffortSection traits={props.traits} /> : null;
}

/**
 * The model picker's footer. Access belongs to the thread, not the model, so it sits apart from
 * the model's options: the mode in use by name, and every mode one press away.
 */
export function ComposerPickerFooter(props: {
  runtimeMode: RuntimeMode;
  runtimeModeOptions: ReadonlyArray<RuntimeModeOption>;
  onRuntimeModeChange: (mode: RuntimeMode) => void;
}) {
  const gt = useGT();
  // Hover or keyboard focus names a mode before a pick.
  const [previewMode, setPreviewMode] = useState<RuntimeMode | null>(null);
  const descriptionId = useId();
  const options = props.runtimeModeOptions;
  const shown = options.find((option) => option.mode === (previewMode ?? props.runtimeMode));
  if (!shown) return null;
  return (
    <div className="flex shrink-0 items-center gap-3 border-t border-border px-3 py-1">
      <span
        aria-hidden="true"
        className={cn(
          "min-w-0 flex-1 truncate text-sm",
          shown.mode === props.runtimeMode ? "text-foreground" : "text-muted-foreground",
        )}
      >
        {shown.label}
      </span>
      <div
        role="radiogroup"
        aria-label={gt("Permissions")}
        data-composer-permissions
        className={SEGMENT_TRACK}
        onKeyDown={(event) => {
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          const modes = Array.from(
            event.currentTarget.querySelectorAll<HTMLElement>('[role="radio"]'),
          );
          const next =
            modes[
              modes.indexOf(event.target as HTMLElement) + (event.key === "ArrowRight" ? 1 : -1)
            ];
          if (!next) return;
          event.preventDefault();
          event.stopPropagation();
          next.focus();
        }}
      >
        {options.map((option) => {
          const OptionIcon = option.icon;
          const checked = option.mode === props.runtimeMode;
          const unavailable = option.disabledReason !== undefined;
          return (
            <Tooltip key={option.mode}>
              <TooltipTrigger
                render={
                  <button
                    type="button"
                    role="radio"
                    aria-checked={checked}
                    data-checked={checked || undefined}
                    aria-label={option.label}
                    aria-describedby={`${descriptionId}-${option.mode}`}
                    // Not `disabled`: a mode the policy rules out still shows why on hover.
                    aria-disabled={unavailable || undefined}
                    tabIndex={checked ? 0 : -1}
                    className={cn(
                      SEGMENT,
                      "w-7 px-0 aria-disabled:cursor-not-allowed aria-disabled:opacity-40",
                    )}
                    onPointerEnter={() => setPreviewMode(option.mode)}
                    onPointerLeave={() => setPreviewMode(null)}
                    onFocus={() => setPreviewMode(option.mode)}
                    onBlur={() => setPreviewMode(null)}
                    onClick={() => {
                      if (!unavailable && !checked) props.onRuntimeModeChange(option.mode);
                    }}
                  />
                }
              >
                <OptionIcon aria-hidden="true" className="size-3.5" />
              </TooltipTrigger>
              <TooltipPopup side="top" className="max-w-56">
                {option.disabledReason ?? option.description}
              </TooltipPopup>
            </Tooltip>
          );
        })}
      </div>
      {options.map((option) => (
        <span key={option.mode} id={`${descriptionId}-${option.mode}`} className="sr-only">
          {option.disabledReason ?? option.description}
        </span>
      ))}
    </div>
  );
}
