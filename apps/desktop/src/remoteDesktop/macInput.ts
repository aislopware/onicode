import type { RemoteDesktopInputEvent } from "@onicode/contracts";

/** A display's frame in the global point space CoreGraphics posts events into. */
export interface DisplayBounds {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

export type MacInput =
  | {
      readonly kind: "mouse";
      /** A `CGEventType`. */
      readonly type: number;
      readonly x: number;
      readonly y: number;
      /** A `CGMouseButton`. */
      readonly button: number;
      readonly clicks: number;
      readonly flags: number;
    }
  | {
      readonly kind: "key";
      readonly keyCode: number;
      readonly down: boolean;
      readonly flags: number;
    }
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "scroll"; readonly dx: number; readonly dy: number; readonly flags: number };

// `CGEventType` and `CGMouseButton` for each `MouseEvent.button`.
const BUTTONS = {
  0: { down: 1, up: 2, dragged: 6, button: 0 },
  1: { down: 25, up: 26, dragged: 27, button: 2 },
  2: { down: 3, up: 4, dragged: 7, button: 1 },
} as const;
const MOUSE_MOVED = 5;

const MODIFIER_FLAGS: Readonly<Record<string, number>> = {
  ShiftLeft: 0x20000,
  ShiftRight: 0x20000,
  ControlLeft: 0x40000,
  ControlRight: 0x40000,
  AltLeft: 0x80000,
  AltRight: 0x80000,
  MetaLeft: 0x100000,
  MetaRight: 0x100000,
};

// One text event carries at most 20 UTF-16 units; CGEventKeyboardSetUnicodeString drops the rest.
const TEXT_CHUNK_UNITS = 20;

/** `KeyboardEvent.code` to the macOS virtual key at that position (`kVK_*` in HIToolbox's Events.h). */
const KEY_CODES: Readonly<Record<string, number>> = {
  KeyA: 0x00,
  KeyS: 0x01,
  KeyD: 0x02,
  KeyF: 0x03,
  KeyH: 0x04,
  KeyG: 0x05,
  KeyZ: 0x06,
  KeyX: 0x07,
  KeyC: 0x08,
  KeyV: 0x09,
  IntlBackslash: 0x0a,
  KeyB: 0x0b,
  KeyQ: 0x0c,
  KeyW: 0x0d,
  KeyE: 0x0e,
  KeyR: 0x0f,
  KeyY: 0x10,
  KeyT: 0x11,
  Digit1: 0x12,
  Digit2: 0x13,
  Digit3: 0x14,
  Digit4: 0x15,
  Digit6: 0x16,
  Digit5: 0x17,
  Equal: 0x18,
  Digit9: 0x19,
  Digit7: 0x1a,
  Minus: 0x1b,
  Digit8: 0x1c,
  Digit0: 0x1d,
  BracketRight: 0x1e,
  KeyO: 0x1f,
  KeyU: 0x20,
  BracketLeft: 0x21,
  KeyI: 0x22,
  KeyP: 0x23,
  Enter: 0x24,
  KeyL: 0x25,
  KeyJ: 0x26,
  Quote: 0x27,
  KeyK: 0x28,
  Semicolon: 0x29,
  Backslash: 0x2a,
  Comma: 0x2b,
  Slash: 0x2c,
  KeyN: 0x2d,
  KeyM: 0x2e,
  Period: 0x2f,
  Tab: 0x30,
  Space: 0x31,
  Backquote: 0x32,
  Backspace: 0x33,
  Escape: 0x35,
  MetaRight: 0x36,
  MetaLeft: 0x37,
  ShiftLeft: 0x38,
  AltLeft: 0x3a,
  ControlLeft: 0x3b,
  ShiftRight: 0x3c,
  AltRight: 0x3d,
  ControlRight: 0x3e,
  F17: 0x40,
  NumpadDecimal: 0x41,
  NumpadMultiply: 0x43,
  NumpadAdd: 0x45,
  NumLock: 0x47,
  NumpadDivide: 0x4b,
  NumpadEnter: 0x4c,
  NumpadSubtract: 0x4e,
  F18: 0x4f,
  F19: 0x50,
  NumpadEqual: 0x51,
  Numpad0: 0x52,
  Numpad1: 0x53,
  Numpad2: 0x54,
  Numpad3: 0x55,
  Numpad4: 0x56,
  Numpad5: 0x57,
  Numpad6: 0x58,
  Numpad7: 0x59,
  F20: 0x5a,
  Numpad8: 0x5b,
  Numpad9: 0x5c,
  F5: 0x60,
  F6: 0x61,
  F7: 0x62,
  F3: 0x63,
  F8: 0x64,
  F9: 0x65,
  F11: 0x67,
  F13: 0x69,
  F16: 0x6a,
  F14: 0x6b,
  F10: 0x6d,
  F12: 0x6f,
  F15: 0x71,
  Home: 0x73,
  PageUp: 0x74,
  Delete: 0x75,
  F4: 0x76,
  End: 0x77,
  F2: 0x78,
  PageDown: 0x79,
  F1: 0x7a,
  ArrowLeft: 0x7b,
  ArrowRight: 0x7c,
  ArrowDown: 0x7d,
  ArrowUp: 0x7e,
};

/**
 * Turns one viewer's input into CoreGraphics events, remembering what it holds down so a
 * viewer that drops mid-drag or mid-chord cannot leave a button or modifier stuck on the host.
 */
export function makeMacInputTranslator() {
  const heldButtons = new Set<0 | 1 | 2>();
  const heldKeys = new Map<string, number>();
  let flags = 0;
  let point = { x: 0, y: 0 };

  const locate = (event: { readonly x: number; readonly y: number }, bounds: DisplayBounds) => {
    point = { x: bounds.x + event.x * bounds.width, y: bounds.y + event.y * bounds.height };
    return point;
  };
  const key = (code: string, keyCode: number, down: boolean): MacInput => {
    const modifier = MODIFIER_FLAGS[code];
    if (down) heldKeys.set(code, keyCode);
    else heldKeys.delete(code);
    if (modifier !== undefined) {
      // The other side of a pair keeps the flag: releasing ShiftLeft while ShiftRight is down.
      const stillHeld = [...heldKeys.keys()].some((held) => MODIFIER_FLAGS[held] === modifier);
      flags = stillHeld ? flags | modifier : flags & ~modifier;
    }
    return { kind: "key", keyCode, down, flags };
  };

  return {
    translate(event: RemoteDesktopInputEvent, bounds: DisplayBounds): ReadonlyArray<MacInput> {
      switch (event.type) {
        case "move": {
          const dragging = heldButtons.values().next().value;
          const { x, y } = locate(event, bounds);
          const type = dragging === undefined ? MOUSE_MOVED : BUTTONS[dragging].dragged;
          const button = dragging === undefined ? 0 : BUTTONS[dragging].button;
          return [{ kind: "mouse", type, x, y, button, clicks: 0, flags }];
        }
        case "button": {
          if (event.down === heldButtons.has(event.button)) return [];
          if (event.down) heldButtons.add(event.button);
          else heldButtons.delete(event.button);
          const { x, y } = locate(event, bounds);
          const mapping = BUTTONS[event.button];
          const type = event.down ? mapping.down : mapping.up;
          return [
            { kind: "mouse", type, x, y, button: mapping.button, clicks: event.clicks, flags },
          ];
        }
        case "scroll":
          // CoreGraphics counts a positive wheel as up and left; the DOM counts it as down and right.
          return [{ kind: "scroll", dx: -Math.round(event.dx), dy: -Math.round(event.dy), flags }];
        case "key": {
          const keyCode = KEY_CODES[event.code];
          if (keyCode === undefined || (!event.down && !heldKeys.has(event.code))) return [];
          return [key(event.code, keyCode, event.down)];
        }
        case "text": {
          const inputs: MacInput[] = [];
          for (let start = 0; start < event.text.length;) {
            let end = Math.min(start + TEXT_CHUNK_UNITS, event.text.length);
            // Never split a surrogate pair across two events.
            const last = event.text.charCodeAt(end - 1);
            if (end < event.text.length && last >= 0xd800 && last <= 0xdbff) end -= 1;
            inputs.push({ kind: "text", text: event.text.slice(start, end) });
            start = end;
          }
          return inputs;
        }
      }
    },
    /** Releases everything this viewer still holds. */
    release(): ReadonlyArray<MacInput> {
      const inputs: MacInput[] = [];
      for (const button of heldButtons) {
        const mapping = BUTTONS[button];
        inputs.push({
          kind: "mouse",
          type: mapping.up,
          ...point,
          button: mapping.button,
          clicks: 1,
          flags,
        });
      }
      heldButtons.clear();
      // Plain keys first, so no key-up arrives after its modifiers have gone.
      const keys = [...heldKeys].sort(
        ([a], [b]) => Number(a in MODIFIER_FLAGS) - Number(b in MODIFIER_FLAGS),
      );
      for (const [code, keyCode] of keys) inputs.push(key(code, keyCode, false));
      return inputs;
    },
  };
}

export type MacInputTranslator = ReturnType<typeof makeMacInputTranslator>;
