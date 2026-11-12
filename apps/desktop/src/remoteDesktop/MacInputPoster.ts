import type { MacInput } from "./macInput.ts";

const CORE_GRAPHICS = "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics";
const HID_EVENT_TAP = 0;
const MOUSE_EVENT_CLICK_STATE = 1;
const SCROLL_UNIT_PIXEL = 0;

let poster: Promise<(input: MacInput) => void> | undefined;

/** Posts events at the HID tap, where they are indistinguishable from the user's own devices. */
export function loadMacInputPoster(): Promise<(input: MacInput) => void> {
  poster ??= import("ffi-rs").then(({ DataType, load, open }) => {
    const library = "oni-coregraphics";
    open({ library, path: CORE_GRAPHICS });
    // CoreFoundation pointers travel as integers; ffi-rs externals would need freeing one by one.
    const pointer = DataType.BigInt;
    const call = <T = void>(
      funcName: string,
      retType: (typeof DataType)[keyof typeof DataType],
      paramsType: ReadonlyArray<(typeof DataType)[keyof typeof DataType]>,
      paramsValue: ReadonlyArray<unknown>,
    ) =>
      load({
        library,
        funcName,
        retType,
        paramsType: [...paramsType],
        paramsValue: [...paramsValue],
      } as Parameters<typeof load>[0]) as T;
    const post = (event: bigint, flags: number | null) => {
      if (event === 0n) return;
      if (flags !== null) {
        call("CGEventSetFlags", DataType.Void, [pointer, DataType.BigInt], [event, BigInt(flags)]);
      }
      call("CGEventPost", DataType.Void, [DataType.I32, pointer], [HID_EVENT_TAP, event]);
      call("CFRelease", DataType.Void, [pointer], [event]);
    };
    const keyboardEvent = (keyCode: number, down: boolean) =>
      call<bigint>(
        "CGEventCreateKeyboardEvent",
        pointer,
        [pointer, DataType.I32, DataType.I32],
        [0n, keyCode, down ? 1 : 0],
      );
    return (input) => {
      switch (input.kind) {
        case "mouse": {
          // CGPoint is two doubles, which every macOS ABI passes exactly as two double arguments.
          const event = call<bigint>(
            "CGEventCreateMouseEvent",
            pointer,
            [pointer, DataType.I32, DataType.Double, DataType.Double, DataType.I32],
            [0n, input.type, input.x, input.y, input.button],
          );
          if (event !== 0n && input.clicks > 0) {
            call(
              "CGEventSetIntegerValueField",
              DataType.Void,
              [pointer, DataType.I32, DataType.BigInt],
              [event, MOUSE_EVENT_CLICK_STATE, BigInt(input.clicks)],
            );
          }
          return post(event, input.flags);
        }
        case "key":
          return post(keyboardEvent(input.keyCode, input.down), input.flags);
        case "scroll":
          return post(
            // The variadic CGEventCreateScrollWheelEvent cannot be called through FFI on arm64.
            call<bigint>(
              "CGEventCreateScrollWheelEvent2",
              pointer,
              [pointer, DataType.I32, DataType.I32, DataType.I32, DataType.I32, DataType.I32],
              [0n, SCROLL_UNIT_PIXEL, 2, input.dy, input.dx, 0],
            ),
            input.flags,
          );
        case "text": {
          const units = Buffer.from(input.text, "utf16le");
          for (const down of [true, false]) {
            const event = keyboardEvent(0, down);
            if (event === 0n) return;
            call(
              "CGEventKeyboardSetUnicodeString",
              DataType.Void,
              [pointer, DataType.BigInt, DataType.U8Array],
              [event, BigInt(units.length / 2), units],
            );
            // No modifiers: a held Shift must not turn typed text into a shortcut.
            post(event, 0);
          }
        }
      }
    };
  });
  return poster;
}
