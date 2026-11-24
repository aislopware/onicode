import { requireNativeView } from "expo";
import { useGT } from "gt-react-native";
import type { PropsWithChildren } from "react";
import type { NativeSyntheticEvent, ViewProps } from "react-native";

import type { HardwareKeyboardCommand } from "../features/keyboard/hardwareKeyboardCommands";

interface NativeKeyboardCommandsProps extends ViewProps {
  readonly enabledCommands: ReadonlyArray<HardwareKeyboardCommand>;
  /** iPad shortcut overlay titles by command id; native keeps English for missing ones. */
  readonly titles: Readonly<Record<string, string>>;
  readonly onCommand: (
    event: NativeSyntheticEvent<{ readonly command: HardwareKeyboardCommand }>,
  ) => void;
}

const NativeKeyboardCommands =
  requireNativeView<NativeKeyboardCommandsProps>("OniKeyboardCommands");

export function OniKeyboardCommands(
  props: PropsWithChildren<{
    readonly enabledCommands: ReadonlyArray<HardwareKeyboardCommand>;
    readonly onCommand: (command: HardwareKeyboardCommand) => void;
  }>,
) {
  const gt = useGT();
  const titles = {
    newTask: gt("New Task"),
    find: gt("Find", { $context: "keyboard shortcut that moves focus to the search field" }),
    focusSearch: gt("Focus Search"),
    commandPalette: gt("Command Palette"),
    paletteNext: gt("Next Result"),
    palettePrevious: gt("Previous Result"),
    paletteDismiss: gt("Close Command Palette"),
    back: gt("Back"),
    files: gt("Open Files"),
    terminal: gt("Open Terminal"),
    review: gt("Open Review"),
    copyThreadReference: gt("Copy PR Link or Thread"),
    toggleSidebar: gt("Toggle Sidebar"),
    cycleHost: gt("Next Machine"),
    threadJump: gt("Go to Thread {index}", { index: "{index}" }),
  };
  return (
    <NativeKeyboardCommands
      onCommand={(event) => props.onCommand(event.nativeEvent.command)}
      enabledCommands={props.enabledCommands}
      titles={titles}
      style={{ flex: 1 }}
    >
      {props.children}
    </NativeKeyboardCommands>
  );
}
