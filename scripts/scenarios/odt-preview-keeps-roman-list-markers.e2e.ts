import { expect } from "e2e";
import { strToU8, zipSync } from "fflate";

import { test, WEB } from "./onicode.ts";

test(
  "An ODT preview keeps a list's uppercase Roman numeral markers",
  WEB,
  async ({ onicode, app, browser, screen }) => {
    const document = zipSync(
      {
        mimetype: strToU8("application/vnd.oasis.opendocument.text"),
        "content.xml": strToU8(
          `<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
          xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
          xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.3">
          <office:automatic-styles><text:list-style style:name="Roman">
            <text:list-level-style-number text:level="1" style:num-format="I" style:num-suffix="."/>
          </text:list-style></office:automatic-styles>
          <office:body><office:text><text:list text:style-name="Roman">
            <text:list-item><text:p>Prepare the site</text:p></text:list-item>
            <text:list-item><text:p>Inspect the result</text:p></text:list-item>
          </text:list></office:text></office:body>
        </office:document-content>`,
        ),
      },
      { level: 0 },
    );
    const project = await onicode.project("demo", { "procedure.odt": document });
    const thread = await onicode.thread({
      project,
      title: "Review the procedure",
      messages: ["Start"],
    });
    await onicode.open(thread);
    await screen.getByText("fixture turn complete", { exact: false }).waitFor();
    await browser.keyboard.press("ControlOrMeta+p");
    await screen.getByPlaceholder("Search files…").fill("procedure.odt");
    await screen.getByRole("option", "procedure.odt", { exact: false }).tap();

    const page = browser.frameLocator('iframe[title^="procedure.odt"]');
    await page.getByText("Prepare the site").waitFor({ timeout: 120_000 });
    await app.screenshot("procedure-list");
    await expect(page.locator('ol[type="I"]')).toHaveCount(1);
  },
);
