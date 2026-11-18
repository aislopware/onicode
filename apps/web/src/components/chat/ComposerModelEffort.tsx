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
import { ChevronRightIcon, ChevronUpIcon, WorkflowIcon } from "lucide-react";
import {
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
  use,
  useRef,
  useState,
} from "react";
import { msg, T, useGT, useMessages } from "gt-react";

import { m, type MessageResolver } from "~/i18n/messages";
import { cn } from "~/lib/utils";
import {
  Menu,
  MenuPopup,
  MenuRadioGroup,
  MenuRadioItem,
  MenuRadioItemIndicator,
  MenuTrigger,
} from "../ui/menu";
import { Switch } from "../ui/switch";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import type { ComposerControlSize } from "./ComposerControl";
import { ComposerPermissionsSection, type RuntimeModeOption } from "./ComposerFooterControls";
import { composerFloatingLayerProps, focusCheckedPermission } from "./composerEventScope";
import { EffortGlyph, FastBolt, resolveEffortGlyph } from "./EffortGlyph";
import { EffortSliderField, effortHue, effortTone } from "./EffortSliderField";
import { DockedPickerContext } from "./ProviderModelPicker";
import {
  getTraitsSectionVisibility,
  type TraitsSelectionInput,
  ULTRATHINK_IN_BODY_NOTE,
  useTraitsSelection,
} from "./TraitsPicker";

type SelectDescriptor = Extract<ProviderOptionDescriptor, { type: "select" }>;
type BooleanDescriptor = Extract<ProviderOptionDescriptor, { type: "boolean" }>;

/** Service tiers, Claude's Fast mode, and Ultracode sit in the footer, not the section. */
function splitSpeed(
  selects: ReadonlyArray<SelectDescriptor>,
  toggles: ReadonlyArray<BooleanDescriptor>,
) {
  const speedSelect = selects.find((descriptor) => descriptor.id === "serviceTier") ?? null;
  const fastToggle = toggles.find((descriptor) => descriptor.id === "fastMode") ?? null;
  const ultracodeToggle = toggles.find((descriptor) => descriptor.id === "ultracode") ?? null;
  return {
    speedSelect,
    fastToggle,
    ultracodeToggle,
    columnSelects: selects.filter((descriptor) => descriptor !== speedSelect),
    columnToggles: toggles.filter(
      (descriptor) => descriptor !== fastToggle && descriptor !== ultracodeToggle,
    ),
  };
}

type ColumnControl =
  | { kind: "select"; descriptor: SelectDescriptor }
  | { kind: "boolean"; descriptor: BooleanDescriptor };

/**
 * The section lists the model's reasoning levels under the header that names it. Other options,
 * such as an agent's session mode, sit below as one row each.
 */
function columnControls(
  selects: ReadonlyArray<SelectDescriptor>,
  toggles: ReadonlyArray<BooleanDescriptor>,
) {
  const { columnSelects, columnToggles } = splitSpeed(selects, toggles);
  const controls: ReadonlyArray<ColumnControl> = [
    ...columnSelects.map((descriptor) => ({ kind: "select" as const, descriptor })),
    ...columnToggles.map((descriptor) => ({ kind: "boolean" as const, descriptor })),
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

const COLUMN_ROW =
  "flex h-8 w-full shrink-0 cursor-pointer items-center gap-2 rounded-sm px-2 text-left text-sm text-foreground outline-none hover:bg-foreground/[0.09] focus-visible:bg-foreground/[0.09] disabled:cursor-not-allowed disabled:opacity-64";
const COLUMN_LABEL = "px-2 pt-1.5 pb-1 font-medium text-muted-foreground text-xs";

/** The track a group of segments sits in; the picker's footer uses the same one. */
const SEGMENT_TRACK = "flex rounded-lg bg-input/40 p-0.5";
const SEGMENT =
  "flex h-6 cursor-pointer items-center justify-center gap-1 rounded-md px-2 text-muted-foreground text-xs outline-none hover:text-foreground data-checked:bg-input/72 data-checked:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset disabled:cursor-not-allowed disabled:opacity-64";

function LevelSegment(props: {
  label: string;
  description?: string | undefined;
  selected: boolean;
  /** The levels' one tab stop: the selected level, else the first when none shows as selected. */
  tabStop: boolean;
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
    "data-effort-level": true,
    // Left and Right move between levels.
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
 * Left and Right walk the levels, which sit in one row. Down goes on to the option rows under
 * them and Up comes back; Up from the levels is left for the popup, which returns to the list.
 * A slider moves its own thumb on Left and Right, so only Down is taken from it.
 */
function moveBetweenRows(event: KeyboardEvent<HTMLElement>) {
  const { key } = event;
  if (key !== "ArrowDown" && key !== "ArrowUp" && key !== "ArrowLeft" && key !== "ArrowRight")
    return;
  // An option's side menu is a React child here, but it moves through its own items.
  if (!event.currentTarget.contains(event.target as Node)) return;
  const rows = Array.from(
    event.currentTarget.querySelectorAll<HTMLElement>("[data-effort-row]:not(:disabled)"),
  );
  const levels = rows.filter((row) => row.matches("[data-effort-level]"));
  const options = rows.filter((row) => !row.matches("[data-effort-level]"));
  const active = document.activeElement;
  const levelIndex = levels.findIndex((row) => row === active);
  const optionIndex = options.findIndex((row) => row === active);
  let next: HTMLElement | undefined;
  if (levelIndex >= 0) {
    if (key === "ArrowUp") return;
    if (key !== "ArrowDown" && active instanceof HTMLInputElement) return;
    next =
      key === "ArrowDown"
        ? options[0]
        : levels[
            Math.min(levels.length - 1, Math.max(0, levelIndex + (key === "ArrowRight" ? 1 : -1)))
          ];
  } else if (optionIndex >= 0) {
    // Left and Right belong to the row: they open and close its menu.
    if (key === "ArrowLeft" || key === "ArrowRight") return;
    if (key === "ArrowDown") next = options[optionIndex + 1] ?? options[optionIndex];
    else if (optionIndex > 0) next = options[optionIndex - 1];
    else if (levels.length > 0)
      next = levels.find((row) => row.getAttribute("aria-checked") === "true") ?? levels[0];
    else return;
  } else {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  if (!next) return;
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

/**
 * The reasoning levels as one slider: a stop per level, and a lit field from the fast end up to
 * the thumb.
 */
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
      className="group/effort flex items-center gap-2.5 text-2xs text-muted-foreground/80"
    >
      <span aria-hidden="true">Faster</span>
      <Slider.Control className="effort-slider-track relative flex h-7 min-w-0 flex-1 cursor-pointer touch-none items-center rounded-lg select-none data-disabled:cursor-not-allowed data-disabled:opacity-64">
        <EffortSliderField level={shown} top={last} tone={props.tone} />
        {/* Inset by half the thumb and the track's edge, so the thumb stays inside at both ends. */}
        <Slider.Track className="relative mx-3 h-6 flex-1">
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
                  {/* The lit field covers the stops it has passed. */}
                  <span
                    className={cn(
                      "size-0.75 rounded-full bg-foreground/30 transition-opacity duration-150",
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
            className={cn(
              "effort-slider-thumb z-10 h-6 w-5 rounded-md transition-[left,scale] duration-150 ease-out group-data-dragging/effort:scale-108 has-focus-visible:outline-2 has-focus-visible:-outline-offset-2 has-focus-visible:outline-ring motion-reduce:transition-none",
              props.shownIndex < 0 && "invisible",
            )}
          />
        </Slider.Track>
      </Slider.Control>
      <span aria-hidden="true">Smarter</span>
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
  const [seen, setSeen] = useState({ index: props.index, from: 0 });
  if (seen.index !== props.index) {
    setSeen({ index: props.index, from: props.index > seen.index ? 1 : -1 });
  }
  return (
    <>
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
      {props.isDefault && props.name !== "Default" ? (
        <span className="text-muted-foreground/70">Default</span>
      ) : null}
    </>
  );
}

/** The section's frame: its label with the picked level beside it, the model, then the rows. */
function EffortFrame(props: { label: string; level?: ReactNode; children: ReactNode }) {
  const picker = use(DockedPickerContext);
  return (
    <>
      <div className="flex shrink-0 items-baseline gap-1.5 px-3 pt-2.5 pb-1.5 text-xs">
        <span className="font-medium text-muted-foreground">{props.label}</span>
        {props.level}
        <span className="ms-auto min-w-0 truncate ps-2 text-muted-foreground/70">
          {picker.modelLabel}
        </span>
      </div>
      <div
        className="flex min-h-0 flex-col overflow-y-auto overscroll-contain px-3 pb-2"
        // Capture, so Up and Down on an option row move rows instead of opening its menu.
        onKeyDownCapture={moveBetweenRows}
      >
        {props.children}
      </div>
    </>
  );
}

function EffortSection(props: { traits: TraitsSelectionInput }) {
  const gt = useGT();
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
              <div className="px-2 pb-1.5 text-muted-foreground/80 text-xs">{value}</div>
            </div>
          ) : null;
        })}
      </EffortFrame>
    );
  }
  if (!effort && others.length === 0) {
    return (
      <EffortFrame label={label}>
        <NoEffortRow />
      </EffortFrame>
    );
  }
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
            <div className="px-1 pb-1.5 text-muted-foreground/80 text-xs">
              {ULTRATHINK_IN_BODY_NOTE}
            </div>
          ) : null}
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
        </>
      ) : effort ? (
        <EffortRows control={effort} traits={traits} locked={locked} />
      ) : null}
      {others.length > 0 ? (
        <div className={cn("flex flex-col gap-0.5", effort && "mt-2")}>
          {others.map((control, index) =>
            control.kind === "boolean" ? (
              <OptionSwitchRow
                key={control.descriptor.id}
                descriptor={control.descriptor}
                traits={traits}
                tabStop={(!effort || locked) && index === 0}
              />
            ) : (
              <OptionMenuRow
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
        <div className="px-1 pb-1.5 text-muted-foreground/80 text-xs">
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

const OPTION_ROW = cn(COLUMN_ROW, "h-auto py-1");

/** An option's name over its current value. */
function OptionRowText(props: { label: string; value: string }) {
  return (
    <span className="flex min-w-0 flex-1 flex-col">
      <span className="truncate text-muted-foreground text-xs">{props.label}</span>
      <span className="truncate">{props.value}</span>
    </span>
  );
}

/**
 * One select besides reasoning, such as an agent's session mode. It names the current choice and
 * opens the rest in a side menu, so any number of options leaves the levels in place.
 */
function OptionMenuRow(props: {
  descriptor: SelectDescriptor;
  traits: TraitsSelection;
  /** Holds the section's tab stop when the model has no levels. */
  tabStop: boolean;
}) {
  const gt = useGT();
  const { descriptor, traits } = props;
  const [open, setOpen] = useState(false);
  const popupRef = useRef<HTMLDivElement>(null);
  const choices = selectableProviderOptionChoices(descriptor);
  const value = traits.selectValue(descriptor);
  const current = choices.find((choice) => choice.id === value)?.label ?? "";
  return (
    <Menu
      open={open}
      // A modal menu would cover the picker, so the next click there would only close it.
      modal={false}
      onOpenChange={setOpen}
      onOpenChangeComplete={(isOpen) => {
        if (!isOpen) return;
        const checked = popupRef.current?.querySelector<HTMLElement>(
          '[role="menuitemradio"][aria-checked="true"]',
        );
        checked?.focus({ preventScroll: true });
        checked?.scrollIntoView({ block: "nearest" });
      }}
    >
      {/* No hover opening: an opened menu takes focus, which would steal typing from search. */}
      <MenuTrigger
        render={
          <button
            type="button"
            data-effort-row
            tabIndex={props.tabStop ? 0 : -1}
            aria-label={`${descriptor.label}: ${current}`}
            className={cn(OPTION_ROW, "data-popup-open:bg-foreground/[0.09]")}
            onKeyDown={(event) => {
              if (event.key !== "ArrowRight") return;
              event.preventDefault();
              event.stopPropagation();
              setOpen(true);
            }}
          />
        }
      >
        <OptionRowText label={descriptor.label} value={current} />
        <ChevronRightIcon
          aria-hidden="true"
          className="-me-1 size-3.5 shrink-0 text-muted-foreground"
        />
      </MenuTrigger>
      <MenuPopup
        ref={popupRef}
        side="right"
        align="start"
        sideOffset={6}
        // A narrow window leaves no room beside the popup, so fall below or above.
        collisionAvoidance={{ fallbackAxisSide: "end" }}
        className="max-w-72"
        data-model-picker-option-menu
        {...composerFloatingLayerProps}
        onKeyDown={(event) => {
          if (event.key !== "ArrowLeft") return;
          event.preventDefault();
          event.stopPropagation();
          setOpen(false);
        }}
      >
        <MenuRadioGroup
          aria-label={descriptor.label}
          value={value}
          onValueChange={(next: string) => {
            if (next !== value) traits.changeSelect(descriptor, next);
          }}
        >
          {choices.map((choice) => (
            <MenuRadioItem key={choice.id} value={choice.id} closeOnClick>
              <span className="flex items-center gap-3">
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate">{choice.label}</span>
                  {choice.description ? (
                    <span className="line-clamp-2 text-muted-foreground text-xs">
                      {choice.description}
                    </span>
                  ) : null}
                </span>
                {choice.isDefault ? (
                  <span className="text-muted-foreground/70 text-xs">
                    {gt("Default", { $context: "marks the default option" })}
                  </span>
                ) : null}
                <MenuRadioItemIndicator />
              </span>
            </MenuRadioItem>
          ))}
        </MenuRadioGroup>
      </MenuPopup>
    </Menu>
  );
}

/** A two-state option toggles in place; a menu would add a click for nothing. */
function OptionSwitchRow(props: {
  descriptor: BooleanDescriptor;
  traits: TraitsSelection;
  tabStop: boolean;
}) {
  const gt = useGT();
  const { descriptor, traits } = props;
  const checked = descriptor.currentValue === true;
  return (
    <label className={cn(OPTION_ROW, "has-focus-visible:bg-foreground/[0.09]")}>
      <OptionRowText
        label={descriptor.label}
        value={
          checked ? gt("On", { $context: "option state" }) : gt("Off", { $context: "option state" })
        }
      />
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

function NoEffortRow() {
  return (
    <div className="flex h-7 items-center px-1 text-muted-foreground text-sm">
      <T>No effort levels</T>
    </div>
  );
}

/** The model picker's effort section: the selected model's levels and its other options. */
export function ComposerEffortColumn(props: { traits: TraitsSelectionInput | null }) {
  const gt = useGT();
  if (props.traits) return <EffortSection traits={props.traits} />;
  return (
    <EffortFrame label={gt("Effort", { $context: "heading for reasoning effort levels" })}>
      <NoEffortRow />
    </EffortFrame>
  );
}

function SpeedSegment(props: {
  checked: boolean;
  label: string;
  /** The untranslated speed name that picks the bolt, when `label` is translated. */
  speed?: string;
  description?: string | undefined;
  onPick: () => void;
}) {
  const speed = props.speed ?? props.label;
  const buttonProps = {
    type: "button",
    role: "radio",
    "aria-checked": props.checked,
    "data-checked": props.checked || undefined,
    className: SEGMENT,
    onClick: props.onPick,
  } as const;
  const content = (
    <>
      {speed === "Fast" || speed === "Ultrafast" ? (
        <FastBolt ultra={speed === "Ultrafast"} />
      ) : null}
      {props.label}
    </>
  );
  if (!props.description) return <button {...buttonProps}>{content}</button>;
  return (
    <Tooltip>
      <TooltipTrigger render={<button {...buttonProps} />}>{content}</TooltipTrigger>
      <TooltipPopup side="top">{props.description}</TooltipPopup>
    </Tooltip>
  );
}

function SpeedControl(props: { traits: TraitsSelectionInput }) {
  const gt = useGT();
  const traits = useTraitsSelection(props.traits);
  const { speedSelect, fastToggle } = splitSpeed(
    traits.selectDescriptors,
    traits.booleanDescriptors,
  );
  if (traits.modelIsUnavailable || (!speedSelect && !fastToggle)) return null;
  return (
    <div
      role="radiogroup"
      aria-label={speedSelect?.label ?? fastToggle?.label}
      className={SEGMENT_TRACK}
    >
      {speedSelect
        ? selectableProviderOptionChoices(speedSelect).map((option) => (
            <SpeedSegment
              key={option.id}
              checked={option.id === traits.selectValue(speedSelect)}
              label={option.label}
              description={option.description}
              onPick={() => traits.changeSelect(speedSelect, option.id)}
            />
          ))
        : fastToggle
          ? ([false, true] as const).map((value) => (
              <SpeedSegment
                key={String(value)}
                checked={(fastToggle.currentValue === true) === value}
                speed={value ? "Fast" : "Standard"}
                label={
                  value
                    ? gt("Fast", { $context: "response speed" })
                    : gt("Standard", { $context: "response speed" })
                }
                onPick={() => traits.changeBoolean(fastToggle.id, value)}
              />
            ))
          : null}
    </div>
  );
}

function UltracodeControl(props: { traits: TraitsSelectionInput }) {
  const traits = useTraitsSelection(props.traits);
  const { ultracodeToggle } = splitSpeed(traits.selectDescriptors, traits.booleanDescriptors);
  if (traits.modelIsUnavailable || !ultracodeToggle) return null;
  const on = ultracodeToggle.currentValue === true;
  return (
    <div className={SEGMENT_TRACK}>
      <button
        type="button"
        aria-pressed={on}
        data-checked={on || undefined}
        className={SEGMENT}
        onClick={() => traits.changeBoolean(ultracodeToggle.id, !on)}
      >
        <WorkflowIcon aria-hidden="true" className="size-3" />
        {ultracodeToggle.label}
      </button>
    </div>
  );
}

/** Permissions change rarely, so the footer names the mode and nests the choice in a menu. */
function PermissionsMenu(props: {
  runtimeMode: RuntimeMode;
  options: ReadonlyArray<RuntimeModeOption>;
  onChange: (mode: RuntimeMode) => void;
}) {
  const gt = useGT();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const current = props.options.find((option) => option.mode === props.runtimeMode);
  if (!current) return null;
  const CurrentIcon = current.icon;
  return (
    <Menu
      onOpenChangeComplete={(open) => {
        if (open) focusCheckedPermission(triggerRef.current);
      }}
    >
      <MenuTrigger
        render={
          <button
            ref={triggerRef}
            type="button"
            data-composer-permissions-trigger
            aria-label={gt("Permissions: {mode}", { mode: current.label })}
            className="ms-auto flex h-7 cursor-pointer items-center gap-1.5 rounded-md px-2 text-muted-foreground text-xs outline-none hover:bg-foreground/[0.09] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset data-popup-open:bg-foreground/[0.09] data-popup-open:text-foreground"
          />
        }
      >
        <CurrentIcon aria-hidden="true" className="size-3.5" />
        {current.label}
        <ChevronUpIcon aria-hidden="true" className="size-3" />
      </MenuTrigger>
      <MenuPopup side="top" align="end" {...composerFloatingLayerProps}>
        <ComposerPermissionsSection
          runtimeMode={props.runtimeMode}
          runtimeModeOptions={props.options}
          onRuntimeModeChange={props.onChange}
        />
      </MenuPopup>
    </Menu>
  );
}

/** The model picker's footer: speed stays one click away, permissions sit one menu deeper. */
export function ComposerPickerFooter(props: {
  traits: TraitsSelectionInput | null;
  runtimeMode: RuntimeMode;
  runtimeModeOptions: ReadonlyArray<RuntimeModeOption>;
  onRuntimeModeChange: (mode: RuntimeMode) => void;
}) {
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-1.5 px-3 pb-3">
      {props.traits ? <SpeedControl traits={props.traits} /> : null}
      {props.traits ? <UltracodeControl traits={props.traits} /> : null}
      <PermissionsMenu
        runtimeMode={props.runtimeMode}
        options={props.runtimeModeOptions}
        onChange={props.onRuntimeModeChange}
      />
    </div>
  );
}
