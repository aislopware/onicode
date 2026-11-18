// @effect-diagnostics nodeBuiltinImport:off - Effect has no synchronous digest.
import * as NodeCrypto from "node:crypto";

/**
 * The target id a driver knows a panel tab by. Derived, so the bridge keeps no table, and
 * made of hex digits because obu names files after it.
 */
export const driverTargetId = (threadId: string, tabId: string) =>
  NodeCrypto.createHash("sha256")
    .update(`${threadId}\0${tabId}`)
    .digest("hex")
    .slice(0, 32)
    .toUpperCase();

export type PageCommandRoute =
  /** Sent to the page's own CDP session. */
  | "forward"
  /** Answered as done: the panel already owns what it asks for. */
  | "acknowledge"
  /** Answered from the thread's panel tabs, never from the browser's own target list. */
  | "target"
  | "refuse";

// Page-scoped domains. Browser, Storage and SystemInfo reach other tabs, profiles or the host.
const FORWARDED_DOMAINS = new Set([
  "Accessibility",
  "CSS",
  "DOM",
  "DOMSnapshot",
  "DOMStorage",
  "Emulation",
  "Fetch",
  "Input",
  "IO",
  "Log",
  "Network",
  "Overlay",
  "Page",
  "Performance",
  "Runtime",
]);
// Downloads and tab focus belong to the panel.
const ACKNOWLEDGED = new Set(["Page.setDownloadBehavior", "Page.bringToFront"]);
const TARGET_COMMANDS = new Set([
  "Target.getTargets",
  "Target.getTargetInfo",
  "Target.createTarget",
  "Target.closeTarget",
]);
// A tab closes through `Target.closeTarget`, which the panel sees; the cookie read is browser-wide.
const REFUSED = new Set(["Page.close", "Page.crash", "Network.getAllCookies"]);

export const routePageCommand = (method: string): PageCommandRoute => {
  if (TARGET_COMMANDS.has(method)) return "target";
  if (ACKNOWLEDGED.has(method)) return "acknowledge";
  if (REFUSED.has(method)) return "refuse";
  return FORWARDED_DOMAINS.has(method.slice(0, method.indexOf("."))) ? "forward" : "refuse";
};

/** The panel shows http and https pages; `about:blank` is what a driver's new tab starts on. */
export const isPanelUrl = (url: string) => {
  if (url === "about:blank") return true;
  try {
    const { protocol } = new URL(url);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
};

/** Playwright wraps a CDP error as `Protocol error (Method): message` with a call log below. */
export const cdpErrorMessage = (cause: unknown) => {
  const message = cause instanceof Error ? cause.message : String(cause);
  return (message.split("\n")[0] ?? "").replace(/^(?:\w+\.\w+: )?Protocol error \([^)]*\): /, "");
};
