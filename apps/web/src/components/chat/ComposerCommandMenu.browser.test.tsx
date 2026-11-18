import { expect, it, vi } from "vite-plus/test";

import { act, render } from "../../../test/render";
import { ComposerCommandMenu, type ComposerCommandItem } from "./ComposerCommandMenu";

const items: ComposerCommandItem[] = Array.from({ length: 12 }, (_, index) => ({
  id: `command-${index}`,
  type: "slash-command",
  command: "clear",
  label: `/command-${index}`,
  description: "Description",
}));

it("keeps the list still for a row the pointer highlights and scrolls to one the keyboard reaches", async () => {
  const onHighlight = vi.fn();
  const menu = (activeItemId: string | null) => (
    <div data-scroller style={{ height: 60, overflow: "auto" }}>
      <ComposerCommandMenu
        listId="suggestions"
        items={items}
        resolvedTheme="light"
        isLoading={false}
        triggerKind="slash-command"
        activeItemId={activeItemId}
        onHighlightedItemChange={onHighlight}
        onSelect={vi.fn()}
      />
    </div>
  );
  await using screen = await render(menu(null));
  const scroller = screen.container.querySelector<HTMLElement>("[data-scroller]")!;
  const row = (index: number) =>
    scroller.querySelector<HTMLElement>(`[data-composer-item-id="command-${index}"]`)!;
  const edge = scroller.getBoundingClientRect().bottom;
  const cutOff = items.findIndex((_, index) => row(index).getBoundingClientRect().bottom > edge);

  await act(() => row(cutOff).dispatchEvent(new MouseEvent("mousemove", { bubbles: true })));
  expect(onHighlight).toHaveBeenLastCalledWith(`command-${cutOff}`);
  await screen.rerender(menu(`command-${cutOff}`));
  expect(scroller.scrollTop).toBe(0);

  await screen.rerender(menu(`command-${cutOff + 1}`));
  expect(scroller.scrollTop).toBeGreaterThan(0);

  // The keyboard coming back to the row the pointer highlighted earlier still reveals it.
  await screen.rerender(menu("command-11"));
  await screen.rerender(menu(`command-${cutOff}`));
  expect(row(cutOff).getBoundingClientRect().top).toBeGreaterThanOrEqual(
    Math.floor(scroller.getBoundingClientRect().top),
  );
});
