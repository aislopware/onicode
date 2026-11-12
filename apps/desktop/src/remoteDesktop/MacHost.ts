// @effect-diagnostics nodeBuiltinImport:off - pbpaste runs outside the main process, so a macOS paste prompt can never stop the process that also posts the viewers' input.
import * as NodeChildProcess from "node:child_process";

const FRAMEWORKS = "/System/Library/Frameworks";
const UTF8 = 0x08000100;
const USER_ACTIVE_LOCAL = 0;
// `NSPasteboardAccessBehavior`, in raw value order.
const PASTEBOARD_ACCESS = ["unasked", "ask", "allowed", "denied"] as const;
const PASTE_PROMPT_TIMEOUT_MS = 60_000;

export type MacClipboardAccess = (typeof PASTEBOARD_ACCESS)[number];

export interface MacHost {
  /** Rises whenever anything copies; reading it shows the user no paste prompt. */
  readonly clipboardChangeCount: () => number;
  /**
   * What macOS does when an app reads the clipboard unasked. "unasked" has shown no prompt yet:
   * the first read shows one, where the system has prompts at all, and then it becomes "ask".
   */
  readonly clipboardAccess: () => MacClipboardAccess;
  /** Reads the clipboard's text in another process, which is the one a paste prompt would hold. */
  readonly readClipboardDetached: () => Promise<string>;
  /** Turns a sleeping display on, as a key press would. */
  readonly wakeDisplay: () => void;
}

let host: Promise<MacHost> | undefined;

/** What a remote desktop host needs from macOS beyond Electron: AppKit's pasteboard and IOKit. */
export function loadMacHost(): Promise<MacHost> {
  host ??= import("ffi-rs").then(({ DataType, load, open }) => {
    open({ library: "oni-appkit", path: `${FRAMEWORKS}/AppKit.framework/AppKit` });
    open({ library: "oni-iokit", path: `${FRAMEWORKS}/IOKit.framework/IOKit` });
    open({
      library: "oni-corefoundation",
      path: `${FRAMEWORKS}/CoreFoundation.framework/CoreFoundation`,
    });
    open({ library: "oni-objc", path: "/usr/lib/libobjc.A.dylib" });
    const pointer = DataType.BigInt;
    const call = <T>(
      library: string,
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
    const selector = (name: string) =>
      call<bigint>("oni-objc", "sel_registerName", pointer, [DataType.String], [name]);
    const message = <T>(
      retType: (typeof DataType)[keyof typeof DataType],
      receiver: bigint,
      name: string,
      argument?: bigint,
    ) =>
      call<T>(
        "oni-objc",
        "objc_msgSend",
        retType,
        argument === undefined ? [pointer, pointer] : [pointer, pointer, pointer],
        argument === undefined ? [receiver, selector(name)] : [receiver, selector(name), argument],
      );
    const pasteboard = message<bigint>(
      pointer,
      call<bigint>("oni-objc", "objc_getClass", pointer, [DataType.String], ["NSPasteboard"]),
      "generalPasteboard",
    );
    // Older systems have no paste prompt, and no way to ask about one.
    const asksBeforePaste = message<boolean>(
      DataType.Boolean,
      pasteboard,
      "respondsToSelector:",
      selector("accessBehavior"),
    );
    const activityName = call<bigint>(
      "oni-corefoundation",
      "CFStringCreateWithCString",
      pointer,
      [pointer, DataType.String, DataType.I32],
      [0n, "Onicode remote desktop", UTF8],
    );
    return {
      clipboardChangeCount: () => Number(message<bigint>(DataType.I64, pasteboard, "changeCount")),
      clipboardAccess: () =>
        asksBeforePaste
          ? (PASTEBOARD_ACCESS[
              Number(message<bigint>(DataType.I64, pasteboard, "accessBehavior"))
            ] ?? "ask")
          : "allowed",
      readClipboardDetached: () =>
        new Promise((resolve) => {
          NodeChildProcess.execFile(
            "/usr/bin/pbpaste",
            ["-Prefer", "txt"],
            {
              encoding: "utf8",
              // pbpaste writes in the locale's encoding, and a GUI app starts with none.
              env: { LC_CTYPE: "UTF-8" },
              maxBuffer: 1024 * 1024,
              timeout: PASTE_PROMPT_TIMEOUT_MS,
            },
            (error, stdout) => resolve(error ? "" : stdout),
          );
        }),
      wakeDisplay: () => {
        // The assertion this creates expires with the display's own idle timer.
        call<number>(
          "oni-iokit",
          "IOPMAssertionDeclareUserActivity",
          DataType.I32,
          [pointer, DataType.I32, DataType.U8Array],
          [activityName, USER_ACTIVE_LOCAL, Buffer.alloc(4)],
        );
      },
    };
  });
  return host;
}
