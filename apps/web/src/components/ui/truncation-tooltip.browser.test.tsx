import { expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

import "~/index.css";
import { render } from "../../../test/render";
import { Tooltip, TooltipPopup, TooltipTrigger } from "./tooltip";
import { TruncationTooltip } from "./truncation-tooltip";

const LONG = "A label that is far too long to fit in a box this narrow";

it("shows cut-off text in full on hover, and nothing for text that fits or explains itself", async () => {
  await using screen = await render(
    <div className="w-40">
      <p className="truncate" data-testid="cut">
        {LONG}
        <span className="sr-only">, for screen readers</span>
      </p>
      <p className="truncate" data-testid="fits">
        Short
      </p>
      <button type="button" className="block w-full" data-testid="focusable">
        <span className="block truncate">{LONG} by keyboard</span>
      </button>
      <Tooltip>
        <TooltipTrigger render={<p className="truncate" data-testid="explained" />}>
          {LONG} again
        </TooltipTrigger>
        <TooltipPopup>Own tooltip</TooltipPopup>
      </Tooltip>
      <TruncationTooltip />
    </div>,
  );
  const tipText = () => document.querySelector("[data-truncation-tooltip]")?.textContent;

  await userEvent.hover(screen.getByTestId("cut"));
  await vi.waitFor(() => expect(tipText()).toBe(LONG));

  await userEvent.hover(screen.getByTestId("fits"));
  await vi.waitFor(() => expect(tipText()).toBeUndefined());

  await userEvent.hover(document.body);
  await userEvent.tab();
  expect(document.activeElement).toBe(screen.getByTestId("focusable").element());
  await vi.waitFor(() => expect(tipText()).toBe(`${LONG} by keyboard`));
  await userEvent.keyboard("{Escape}");
  await vi.waitFor(() => expect(tipText()).toBeUndefined());

  await userEvent.hover(screen.getByTestId("explained"));
  await expect.element(page.getByText("Own tooltip")).toBeVisible();
  await expect.element(page.getByText(`${LONG} again`, { exact: true })).toHaveLength(1);
});
