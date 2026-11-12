import * as Schema from "effect/Schema";

import { PositiveInt, TrimmedNonEmptyString } from "./baseSchemas.ts";

export const RemoteDesktopDisplay = Schema.Struct({
  id: TrimmedNonEmptyString,
  label: Schema.String,
  /** Physical pixels, which is what the stream carries at most. */
  width: PositiveInt,
  height: PositiveInt,
  primary: Schema.Boolean,
});
export type RemoteDesktopDisplay = typeof RemoteDesktopDisplay.Type;

/** The desktop app sharing its machine's screen. Its window has to exist: the app captures and injects. */
export const RemoteDesktopHost = Schema.Struct({
  displays: Schema.Array(RemoteDesktopDisplay),
  /** False where the app cannot inject input: no injector for the OS, or its permission is missing. */
  control: Schema.Boolean,
});
export type RemoteDesktopHost = typeof RemoteDesktopHost.Type;

export const RemoteDesktopState = Schema.Struct({
  host: Schema.NullOr(RemoteDesktopHost),
});
export type RemoteDesktopState = typeof RemoteDesktopState.Type;

const SessionDescription = Schema.String.check(Schema.isMaxLength(64 * 1024));

export const RemoteDesktopConnectInput = Schema.Struct({
  displayId: TrimmedNonEmptyString,
  /** A complete WebRTC offer: every candidate gathered, nothing trickled afterwards. */
  offer: SessionDescription,
});
export type RemoteDesktopConnectInput = typeof RemoteDesktopConnectInput.Type;

export const RemoteDesktopConnectResult = Schema.Struct({
  answer: SessionDescription,
});
export type RemoteDesktopConnectResult = typeof RemoteDesktopConnectResult.Type;

/** What the host receives for each viewer that asks to connect. */
export const RemoteDesktopHostRequest = Schema.Struct({
  requestId: TrimmedNonEmptyString,
  displayId: TrimmedNonEmptyString,
  offer: SessionDescription,
  /** The viewer's auth session id, which the host's "who is watching" list names from Sharing & access. */
  viewer: Schema.String,
});
export type RemoteDesktopHostRequest = typeof RemoteDesktopHostRequest.Type;

export const RemoteDesktopAnswerInput = Schema.Struct({
  requestId: TrimmedNonEmptyString,
  result: Schema.Union([
    Schema.Struct({ answer: SessionDescription }),
    Schema.Struct({ refused: Schema.String }),
  ]),
});
export type RemoteDesktopAnswerInput = typeof RemoteDesktopAnswerInput.Type;

/** A position inside the shared display, 0 to 1 from its top-left corner. */
const Unit = Schema.Finite.check(Schema.isBetween({ minimum: 0, maximum: 1 }));
const WheelDelta = Schema.Finite.check(Schema.isBetween({ minimum: -10_000, maximum: 10_000 }));

export const RemoteDesktopInputEvent = Schema.Union([
  Schema.Struct({ type: Schema.Literal("move"), x: Unit, y: Unit }),
  Schema.Struct({
    type: Schema.Literal("button"),
    /** As `MouseEvent.button`: 0 primary, 1 middle, 2 secondary. */
    button: Schema.Literals([0, 1, 2]),
    down: Schema.Boolean,
    x: Unit,
    y: Unit,
    /** 2 for the presses of a double click, as `MouseEvent.detail`. */
    clicks: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 3 })),
  }),
  /** Pixels, signed as `WheelEvent` deltas: positive scrolls down and right. */
  Schema.Struct({ type: Schema.Literal("scroll"), dx: WheelDelta, dy: WheelDelta }),
  /** A physical key by `KeyboardEvent.code`, so the host's layout decides the character. */
  Schema.Struct({
    type: Schema.Literal("key"),
    code: Schema.String.check(Schema.isMaxLength(32)),
    down: Schema.Boolean,
  }),
  /** Committed text from a soft keyboard or IME, typed as characters whatever the host layout. */
  Schema.Struct({
    type: Schema.Literal("text"),
    text: Schema.String.check(Schema.isMaxLength(512)),
  }),
]);
export type RemoteDesktopInputEvent = typeof RemoteDesktopInputEvent.Type;

/** Clipboard text a host and a viewer exchange; `REMOTE_DESKTOP_CLIPBOARD_LIMIT` is the same number. */
export const RemoteDesktopClipboardText = Schema.String.check(
  Schema.isMinLength(1),
  Schema.isMaxLength(64_000),
);

export const RemoteDesktopInputBatch = Schema.Struct({
  sessionId: TrimmedNonEmptyString,
  displayId: TrimmedNonEmptyString,
  events: Schema.Array(RemoteDesktopInputEvent).check(Schema.isMaxLength(256)),
});
export type RemoteDesktopInputBatch = typeof RemoteDesktopInputBatch.Type;

/** What the desktop app's main process knows about sharing this machine. */
export const RemoteDesktopHostCapabilities = Schema.Struct({
  displays: Schema.Array(RemoteDesktopDisplay),
  /** Null where the OS asks for no permission. */
  screenRecording: Schema.NullOr(Schema.Boolean),
  /** Null where Onicode has no input injector for the OS; false until the OS allows it. */
  inputControl: Schema.NullOr(Schema.Boolean),
  /** Null where the OS never asks; false while it would ask before each clipboard read. */
  clipboardRead: Schema.NullOr(Schema.Boolean),
});
export type RemoteDesktopHostCapabilities = typeof RemoteDesktopHostCapabilities.Type;

export class RemoteDesktopError extends Schema.TaggedError<RemoteDesktopError>()(
  "RemoteDesktopError",
  {
    reason: Schema.Literals(["no-host", "refused", "timeout"]),
    /** Why the host refused, in its words, such as a missing OS permission. */
    refusal: Schema.optionalKey(Schema.String),
  },
) {
  override get message(): string {
    switch (this.reason) {
      case "no-host":
        return "No desktop app is sharing this machine's screen.";
      case "refused":
        return this.refusal ?? "The host refused the connection.";
      case "timeout":
        return "The host did not answer in time.";
    }
  }
}
