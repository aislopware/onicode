import { AtomRegistry } from "effect/reactivity";
import { expect, it, vi } from "vite-plus/test";

import { userPresentAtom } from "./userPresence";

it("counts input a descendant keeps to itself, until it stops for a minute", () => {
  using now = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 3_600_000);
  using _focus = vi.spyOn(document, "hasFocus").mockReturnValue(true);
  const registry = AtomRegistry.make();
  using _mounted = { [Symbol.dispose]: registry.mount(userPresentAtom) };
  const terminal = document.createElement("div");
  terminal.addEventListener("keydown", (event) => event.stopPropagation());
  document.body.append(terminal);
  using _terminal = { [Symbol.dispose]: () => terminal.remove() };
  expect(registry.get(userPresentAtom)).toBe(false);

  terminal.dispatchEvent(new KeyboardEvent("keydown", { key: "a", bubbles: true }));
  expect(registry.get(userPresentAtom)).toBe(true);

  now.mockReturnValue(Date.now() + 60_000);
  window.dispatchEvent(new Event("blur"));
  expect(registry.get(userPresentAtom)).toBe(false);
});
