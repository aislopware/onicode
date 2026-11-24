import { describe, expect, it } from "vite-plus/test";

import { clearSearchOnEscape } from "./searchFieldEscape";

function press(
  init: {
    key?: string;
    isComposing?: boolean;
    keyCode?: number;
    repeat?: boolean;
    defaultPrevented?: boolean;
    shiftKey?: boolean;
    metaKey?: boolean;
  },
  query: string,
) {
  const outcome = { prevented: false, stopped: false, cleared: false };
  clearSearchOnEscape(
    {
      key: init.key ?? "Escape",
      repeat: init.repeat ?? false,
      defaultPrevented: init.defaultPrevented ?? false,
      altKey: false,
      ctrlKey: false,
      metaKey: init.metaKey ?? false,
      shiftKey: init.shiftKey ?? false,
      nativeEvent: { isComposing: init.isComposing ?? false, keyCode: init.keyCode ?? 27 },
      preventDefault: () => {
        outcome.prevented = true;
      },
      stopPropagation: () => {
        outcome.stopped = true;
      },
    },
    query,
    () => {
      outcome.cleared = true;
    },
  );
  return outcome;
}

describe("clearSearchOnEscape", () => {
  it("clears a filled field and keeps Esc from closing the page", () => {
    expect(press({}, "mac")).toEqual({ prevented: true, stopped: true, cleared: true });
  });

  it("lets Esc through from an empty field unless it is held", () => {
    expect(press({}, "")).toEqual({ prevented: false, stopped: false, cleared: false });
    expect(press({ repeat: true }, "")).toEqual({ prevented: true, stopped: true, cleared: false });
  });

  it("lets the Esc that cancels an IME composition cancel it without closing the page", () => {
    const kept = { prevented: false, stopped: true, cleared: false };
    expect(press({ isComposing: true }, "にほ")).toEqual(kept);
    expect(press({ keyCode: 229 }, "")).toEqual(kept);
  });

  it("leaves modified, already handled and other keys alone", () => {
    const untouched = { prevented: false, stopped: false, cleared: false };
    expect(press({ shiftKey: true }, "mac")).toEqual(untouched);
    expect(press({ metaKey: true }, "mac")).toEqual(untouched);
    expect(press({ defaultPrevented: true }, "mac")).toEqual(untouched);
    expect(press({ key: "Enter", keyCode: 13 }, "mac")).toEqual(untouched);
  });
});
