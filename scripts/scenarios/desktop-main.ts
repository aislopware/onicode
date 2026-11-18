// The Electron main process of the scenario app, reached over its Node inspector. CDP on the
// window sees only the renderer; native menus, dialogs and window state live here.
import * as Schema from "effect/Schema";

/** What `start-app.ts desktop` writes once the dev desktop window is up. */
export const DesktopStack = Schema.Struct({
  /** The window's DevTools Protocol port: e2e drives the renderer through it. */
  cdpPort: Schema.Number,
  /** The Node inspector of the Electron main process. */
  inspectPort: Schema.Number,
});

interface InspectorTarget {
  readonly webSocketDebuggerUrl: string;
}

/** Evaluates an expression in the main process and returns its JSON value. */
export const evaluateInMain = async <A>(inspectPort: number, expression: string): Promise<A> => {
  // @effect-diagnostics-next-line globalFetch:off - a promise helper for e2e fixtures, outside any Effect
  const response = await fetch(`http://127.0.0.1:${inspectPort}/json/list`);
  const targets = (await response.json()) as ReadonlyArray<InspectorTarget>;
  const target = targets[0];
  if (target === undefined) throw new Error("The main process exposes no inspector target.");
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  try {
    return await new Promise<A>((resolve, reject) => {
      socket.addEventListener("error", () => reject(new Error("The main inspector failed.")));
      // Settling twice is harmless, and an app that quits mid-call would otherwise never answer.
      socket.addEventListener("close", () => reject(new Error("The main inspector closed.")));
      socket.addEventListener("open", () => {
        socket.send(
          JSON.stringify({
            id: 1,
            method: "Runtime.evaluate",
            params: { expression, awaitPromise: true, returnByValue: true },
          }),
        );
      });
      socket.addEventListener("message", (event) => {
        const message = JSON.parse(String(event.data)) as {
          readonly id?: number;
          readonly result?: {
            readonly result?: { readonly value?: A };
            readonly exceptionDetails?: { readonly exception?: { readonly description?: string } };
          };
        };
        if (message.id !== 1) return;
        const failure = message.result?.exceptionDetails;
        if (failure !== undefined) {
          reject(new Error(failure.exception?.description ?? "The main process threw."));
        } else {
          resolve(message.result?.result?.value as A);
        }
      });
    });
  } finally {
    socket.close();
  }
};

// A native menu or dialog blocks until someone clicks it, and no test can. The stack swaps
// these for queues once: a menu is still the one the app built, with its real click handlers,
// a message box answers with its first button, and a folder picker with the path a test queued.
export const CAPTURE_NATIVE_UI = `(() => {
  const { Menu, dialog } = process.mainModule.require("electron");
  if (globalThis.__e2eMenus) return true;
  globalThis.__e2eMenus = [];
  globalThis.__e2eMessageBoxes = [];
  globalThis.__e2eFolders = [];
  // Captures should not depend on the size the last developer left the dev window at.
  const { BrowserWindow } = process.mainModule.require("electron");
  for (const window of BrowserWindow.getAllWindows()) window.setContentSize(1280, 800);
  Menu.prototype.popup = function (options) {
    globalThis.__e2eMenus.push({ menu: this, options: options ?? {} });
  };
  const options = (args) => args.find((arg) => arg && arg.constructor === Object) ?? {};
  dialog.showMessageBox = async (...args) => {
    const { type, title, message, detail, buttons } = options(args);
    globalThis.__e2eMessageBoxes.push({ type, title, message, detail, buttons });
    return { response: 0, checkboxChecked: false };
  };
  dialog.showOpenDialog = async () => {
    const folder = globalThis.__e2eFolders.shift();
    return folder === undefined ? { canceled: true, filePaths: [] } : { canceled: false, filePaths: [folder] };
  };
  return true;
})()`;

// e2e's own browsers give every attempt a fresh context. The dev window is one profile for the
// whole run, so each attempt starts by emptying what the last one stored and undoing its zoom.
// The page is unloaded first: a page still open would write its state back. Default client
// settings go through the app first: the shell keeps their language in memory for its menus.
export const RESET_WINDOW = `(async () => {
  const { BrowserWindow } = process.mainModule.require("electron");
  for (const window of BrowserWindow.getAllWindows()) {
    if (window.isDestroyed()) continue;
    await window.webContents
      .executeJavaScript("window.desktopBridge?.setClientSettings({})")
      .catch(() => undefined);
    window.webContents.setZoomFactor(1);
    await window.loadURL("about:blank");
    await window.webContents.session.clearStorageData({ storages: ["localstorage", "indexdb"] });
  }
  return true;
})()`;

const MENU_HELPERS = `
  const { Menu, BrowserWindow, nativeTheme } = process.mainModule.require("electron");
  const describe = (menu) => menu.items
    .filter((item) => item.type !== "separator" && item.visible)
    .map((item) => ({
      label: item.label,
      enabled: item.enabled,
      ...(item.type === "checkbox" ? { checked: item.checked } : {}),
      ...(item.submenu ? { submenu: describe(item.submenu) } : {}),
    }));
  const find = (menu, path) => {
    const [label, ...rest] = path;
    const item = menu.items.find((candidate) => candidate.label === label);
    if (!item) throw new Error("No menu item " + JSON.stringify(label) + " among " + JSON.stringify(menu.items.map((candidate) => candidate.label)));
    if (rest.length === 0) return item;
    if (!item.submenu) throw new Error(JSON.stringify(label) + " has no submenu.");
    return find(item.submenu, rest);
  };
  const mainWindow = () => BrowserWindow.getAllWindows().find((window) => !window.isDestroyed() && window.webContents.getURL().startsWith("onicode-dev://app"));
`;

export interface NativeMenuItem {
  readonly label: string;
  readonly enabled: boolean;
  readonly checked?: boolean;
  readonly submenu?: ReadonlyArray<NativeMenuItem>;
}

export interface NativeMessageBox {
  readonly type?: string;
  readonly title?: string;
  readonly message: string;
  readonly detail?: string;
  readonly buttons?: ReadonlyArray<string>;
}

export interface MainWindowState {
  readonly title: string;
  readonly fullScreen: boolean;
  readonly zoomFactor: number;
  readonly windowCount: number;
  /** Electron's nativeTheme.themeSource: what native menus and dialogs follow. */
  readonly themeSource: "system" | "light" | "dark";
}

/** The main process of the stack's dev app. */
export const mainProcess = (inspectPort: number) => {
  const evaluate = <A>(body: string) =>
    evaluateInMain<A>(inspectPort, `(async () => {${MENU_HELPERS}${body}})()`);
  return {
    /** The context menu the app last popped up, or null while none is waiting. */
    contextMenu: () =>
      evaluate<ReadonlyArray<NativeMenuItem> | null>(
        `const entry = globalThis.__e2eMenus.at(-1); return entry ? describe(entry.menu) : null;`,
      ),
    /** Clicks an item of the waiting context menu, by its label path. */
    chooseContextMenuItem: (...path: ReadonlyArray<string>) =>
      evaluate<true>(
        `const entry = globalThis.__e2eMenus.pop();
         if (!entry) throw new Error("No context menu is open.");
         globalThis.__e2eMenus.length = 0;
         find(entry.menu, ${JSON.stringify(path)}).click(undefined, mainWindow(), mainWindow()?.webContents);
         return true;`,
      ),
    /** Closes the waiting context menus without a choice, as Escape would. */
    dismissContextMenus: () =>
      evaluate<true>(
        `for (const entry of globalThis.__e2eMenus.splice(0)) entry.options.callback?.(); return true;`,
      ),
    /** The native message boxes the app has shown since the last call, oldest first. */
    takeMessageBoxes: () =>
      evaluate<ReadonlyArray<NativeMessageBox>>(`return globalThis.__e2eMessageBoxes.splice(0);`),
    /** Answers the next native folder picker with a path; an unanswered one is cancelled. */
    answerFolderPicker: (path: string) =>
      evaluate<true>(`globalThis.__e2eFolders.push(${JSON.stringify(path)}); return true;`),
    forgetFolderPickerAnswers: () =>
      evaluate<true>(`globalThis.__e2eFolders.length = 0; return true;`),
    applicationMenu: () =>
      evaluate<ReadonlyArray<NativeMenuItem>>(`return describe(Menu.getApplicationMenu());`),
    /** Clicks an application menu item, by its label path: ("File", "New Thread"). */
    chooseApplicationMenuItem: (...path: ReadonlyArray<string>) =>
      evaluate<true>(
        `find(Menu.getApplicationMenu(), ${JSON.stringify(path)}).click(undefined, mainWindow(), mainWindow()?.webContents); return true;`,
      ),
    window: () =>
      evaluate<MainWindowState>(
        `const window = mainWindow();
         return {
           title: window.getTitle(),
           fullScreen: window.isFullScreen(),
           zoomFactor: window.webContents.getZoomFactor(),
           windowCount: BrowserWindow.getAllWindows().filter((candidate) => !candidate.isDestroyed()).length,
           themeSource: nativeTheme.themeSource,
         };`,
      ),
  };
};
