import { beforeEach } from "vite-plus/test";
import { commands, page, server } from "vite-plus/test/browser";

import { initializeI18n } from "../src/i18n/initializeI18n";

declare module "vite-plus/test/browser" {
  interface BrowserCommands {
    /** Defined in vite.config.ts. Moves Playwright's mouse off the page. */
    resetPointer: () => Promise<void>;
    /** Defined in vite.config.ts. Clicks without moving Playwright's mouse. */
    pressPointer: () => Promise<void>;
    /** Defined in vite.config.ts. Presses the primary button where the mouse is, without releasing. */
    pointerDown: () => Promise<void>;
    /** Defined in vite.config.ts. Releases the primary button. */
    pointerUp: () => Promise<void>;
  }
}

// Test files run one after another in the same tab, so one file's persisted drafts, settings and
// caches would seed the next file's stores. This runs before the file imports them.
localStorage.clear();
sessionStorage.clear();
await Promise.all(
  (await indexedDB.databases()).map(
    ({ name }) =>
      new Promise<void>((resolve) => {
        if (name === undefined) return resolve();
        const request = indexedDB.deleteDatabase(name);
        // A connection the previous file left open blocks the delete; opens queue behind it.
        request.onsuccess = request.onerror = request.onblocked = () => resolve();
      }),
  ),
);

// A size set with page.viewport outlives its test, so a later test would lay out at it.
const { viewport } = server.config.browser;

// Playwright's mouse outlives each test and each file, so whatever the next test mounts
// under its last position starts hovered. Registered here rather than in render() so tests
// that mount without it are covered too. Before each test, not after: a timed-out test keeps
// running and can move the mouse after its own afterEach.
beforeEach(async () => {
  await commands.resetPointer();
  if (window.innerWidth !== viewport.width || window.innerHeight !== viewport.height)
    await page.viewport(viewport.width, viewport.height);
});

// The app initializes GT at startup; components using its hooks need the same here.
initializeI18n(() => {});
