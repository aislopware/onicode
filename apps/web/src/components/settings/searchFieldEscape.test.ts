import { describe, expect, it } from "vite-plus/test";

import { clearSearchOnEscape } from "./searchFieldEscape";

function press(
  init: { key?: string; isComposing?: boolean; keyCode?: number; repeat?: boolean },
  query: string,
) {
  const outcome = { prevented: false, stopped: false, cleared: false };
  clearSearchOnEscape(
    {
      key: init.key ?? "Escape",
      keyCode: init.keyCode ?? 27,
      repeat: init.repeat ?? false,
      nativeEvent: { isComposing: init.isComposing ?? false },
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

  it("leaves the Esc that cancels an IME composition and other keys alone", () => {
    const untouched = { prevented: false, stopped: false, cleared: false };
    expect(press({ isComposing: true }, "にほ")).toEqual(untouched);
    expect(press({ keyCode: 229 }, "にほ")).toEqual(untouched);
    expect(press({ key: "Enter", keyCode: 13 }, "mac")).toEqual(untouched);
  });
});
