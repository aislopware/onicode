import { describe, expect, it } from "vite-plus/test";

import { zipEntries, zipFixture } from "../../../test/zip";

import { prepareOffice } from "./officeArchive";
import { unlockOpenDocument } from "./openDocumentEncryption";

const W = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"';
const V = 'xmlns:v="urn:schemas-microsoft-com:vml"';
const R = 'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"';
const relationship = (type: string, target: string) =>
  `<Relationship Id="${type}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/${type}" Target="${target}"/>`;
const paragraph = (text: string, properties = "") =>
  `<w:p>${properties}<w:r><w:t xml:space="preserve">${text}</w:t></w:r></w:p>`;

function wordFile(
  body: string,
  parts: Partial<Record<"styles" | "numbering" | "header" | "footer" | "footnotes", string>> = {},
  section = "",
  extraRels = "",
  files: Record<string, string> = {},
) {
  return zipFixture({
    "[Content_Types].xml": `<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="svg" ContentType="image/svg+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>`,
    "_rels/.rels": `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">${relationship("officeDocument", "word/document.xml")}</Relationships>`,
    "word/_rels/document.xml.rels": `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">${Object.keys(
      parts,
    )
      .map((part) => relationship(part, `${part}.xml`))
      .join("")}${extraRels}</Relationships>`,
    ...Object.fromEntries(Object.entries(parts).map(([part, xml]) => [`word/${part}.xml`, xml])),
    ...files,
    "word/document.xml": `<w:document ${W} ${V} ${R}><w:body>${body}<w:sectPr>${section}<w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:body></w:document>`,
  }).buffer;
}

async function prepared(bytes: ArrayBuffer) {
  const result = await prepareOffice(bytes, "docx");
  const zip = zipEntries(result.bytes);
  return { truncated: result.truncated, part: zip.text };
}

// regression: the budgets sized for a main-thread renderer cut ordinary documents short.
describe("Word preview limits", () => {
  it("keeps a document whose table follows a long one", async () => {
    const rows = Array.from(
      { length: 500 },
      (_, row) =>
        `<w:tr>${Array.from({ length: 5 }, (_, cell) => `<w:tc>${paragraph(`r${row}c${cell}`)}</w:tc>`).join("")}</w:tr>`,
    ).join("");
    const result = await prepared(
      wordFile(`<w:tbl>${rows}</w:tbl>${paragraph("After the table")}`),
    );
    expect(result.truncated).toBe(false);
    expect(result.part("word/document.xml")).toContain("After the table");
  });

  it("keeps the list and style definitions of a long document", async () => {
    const levels = Array.from(
      { length: 9 },
      (_, level) =>
        `<w:lvl w:ilvl="${level}"><w:numFmt w:val="decimal"/><w:lvlText w:val="%${level + 1}."/></w:lvl>`,
    ).join("");
    const numbering = `<w:numbering ${W}>${Array.from({ length: 60 }, (_, list) => `<w:abstractNum w:abstractNumId="${list}">${levels}</w:abstractNum>`).join("")}${Array.from({ length: 60 }, (_, list) => `<w:num w:numId="${list + 1}"><w:abstractNumId w:val="${list}"/></w:num>`).join("")}</w:numbering>`;
    const styles = `<w:styles ${W}>${Array.from(
      { length: 1_500 },
      (_, style) =>
        `<w:style w:type="paragraph" w:styleId="S${style}">${style % 10 ? `<w:basedOn w:val="S${style - 1}"/>` : ""}<w:pPr><w:spacing w:after="${style}"/></w:pPr><w:rPr><w:sz w:val="22"/></w:rPr></w:style>`,
    ).join("")}</w:styles>`;
    const result = await prepared(
      wordFile(
        Array.from({ length: 6_000 }, (_, index) =>
          paragraph(
            `Item ${index}`,
            `<w:pPr><w:pStyle w:val="S${index % 1_500}"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="${(index % 60) + 1}"/></w:numPr></w:pPr>`,
          ),
        ).join(""),
        { styles, numbering },
      ),
    );
    expect(result.truncated).toBe(false);
    expect(result.part("word/numbering.xml")).toBe(numbering);
    expect(result.part("word/styles.xml")).toBe(styles);
    expect(result.part("word/document.xml")).toContain("Item 5999");
  });

  it("keeps the lists of a document with picture bullets, with their text bullets", async () => {
    const picture = `<w:numPicBullet w:numPicBulletId="0"><w:pict><v:shape style="width:9pt;height:9pt"><v:imagedata r:id="rId1"/></v:shape></w:pict></w:numPicBullet>`;
    const pictured = `<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="\uf0b7"/><w:lvlPicBulletId w:val="0"/></w:lvl></w:abstractNum>`;
    const numbered = `<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>`;
    const lists = `<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>`;
    const result = await prepared(
      wordFile(paragraph("Item", `<w:pPr><w:numPr><w:numId w:val="2"/></w:numPr></w:pPr>`), {
        numbering: `<w:numbering ${W} ${V} ${R}>${picture}${pictured}${numbered}${lists}</w:numbering>`,
      }),
    );
    expect(result.part("word/numbering.xml")).toBe(
      `<w:numbering ${W} ${V} ${R}>${pictured.replace('<w:lvlPicBulletId w:val="0"/>', "")}${numbered}${lists}</w:numbering>`,
    );
  });

  const text = "x".repeat(140_000);
  it.each([
    ["on its own", `<w:p><w:r><w:t>${text}</w:t></w:r></w:p>`],
    [
      "after an empty paragraph in a text box",
      `<w:p><w:r><w:pict><v:shape><v:textbox><w:txbxContent><w:p/></w:txbxContent></v:textbox></v:shape></w:pict></w:r><w:r><w:t>${text}</w:t></w:r></w:p>`,
    ],
    ["written as CDATA", `<w:p><w:r><w:t><![CDATA[${text}]]></w:t></w:r></w:p>`],
  ])("stops before a paragraph too long to lay out %s", async (_, long) => {
    const result = await prepared(wordFile(paragraph("Kept") + long + paragraph("Not reached")));
    expect(result.truncated).toBe(true);
    const document = result.part("word/document.xml");
    expect(document).toContain("Kept");
    expect(document).not.toContain("Not reached");
  });

  it("counts only the text a paragraph lays out", async () => {
    const formatted = `<w:p>${" ".repeat(140_000)}<w:r><w:instrText>${"x".repeat(140_000)}</w:instrText></w:r><w:r><w:t>${"x".repeat(40_000)}</w:t></w:r></w:p>`;
    const result = await prepared(wordFile(formatted + paragraph("After")));
    expect(result.truncated).toBe(false);
    expect(result.part("word/document.xml")).toContain("After");
  });

  it("keeps a long header", async () => {
    const header = `<w:hdr ${W}>${Array.from({ length: 3_000 }, (_, line) => paragraph(`Header line ${line}`)).join("")}</w:hdr>`;
    const footer = `<w:ftr ${W}>${paragraph("Footer")}</w:ftr>`;
    const footnotes = `<w:footnotes ${W}><w:footnote w:id="1">${paragraph("Note")}</w:footnote></w:footnotes>`;
    const result = await prepared(
      wordFile(
        `<w:p><w:r><w:t>Body</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>`,
        { header, footer, footnotes },
        `<w:headerReference w:type="default" r:id="header"/><w:footerReference w:type="default" r:id="footer"/>`,
      ),
    );
    expect(result.truncated).toBe(false);
    expect(result.part("word/header.xml")).toBe(header);
    expect(result.part("word/footer.xml")).toBe(footer);
    expect(result.part("word/footnotes.xml")).toBe(footnotes);
    expect(result.part("word/document.xml")).toContain("Body");
  });

  it.each([
    ["a malformed external", "hyperlink", "http://[placeholder]/", "External"],
    ["a space-padded malformed", "hyperlink", " http://[placeholder]/", "Internal"],
    ["a UNC", "hyperlink", String.raw`\\server\share\report.docx`, "Internal"],
    ["a malformed UNC", "hyperlink", String.raw`\\[server]\share`, "Internal"],
    ["a malformed internal", "hyperlink", "/&#9;/[placeholder]", "Internal"],
    ["a tab-split protocol-relative", "hyperlink", "/&#9;/example.com/x", "Internal"],
    ["a tab-split javascript", "hyperlink", "javascript&#9;:alert(1)", "Internal"],
    ["a tab-split file", "hyperlink", "file&#9;:///etc/passwd", "Internal"],
    ["a tab-split UNC", "hyperlink", String.raw`\&#9;\server\share`, "Internal"],
    ["a line-split web", "header", "http&#10;://example.com/", "Internal"],
    ["a line-split same-scheme", "header", "http&#13;s:header.xml", "Internal"],
  ])("drops %s %s relationship while keeping body text", async (_, role, target, mode) => {
    const result = await prepared(
      wordFile(
        paragraph("Body text"),
        {},
        "",
        `<Relationship Id="badLink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/${role}" Target="${target}" TargetMode="${mode}"/>`,
      ),
    );
    expect(result.truncated).toBe(false);
    expect(result.part("word/document.xml")).toContain("Body text");
    expect(result.part("word/_rels/document.xml.rels")).not.toContain("badLink");
  });

  // Read as the part list reads it, so the header is counted and its reference kept.
  it("keeps a header whose part name has a literal ?", async () => {
    const header = `<w:hdr ${W}>${paragraph("Header")}</w:hdr>`;
    const result = await prepared(
      wordFile(
        paragraph("Body text"),
        {},
        `<w:headerReference w:type="default" r:id="header"/>`,
        relationship("header", "header.xml?v=1"),
        { "word/header.xml?v=1": header },
      ),
    );
    expect(result.part("word/document.xml")).toContain("w:headerReference");
    expect(result.part("word/header.xml?v=1")).toBe(header);
  });

  // Trimming these spaces would alias the target to the document itself.
  it.each(["&#160;", "&#65279;"])("keeps %s in a relationship target", async (space) => {
    const result = await prepared(
      wordFile(
        paragraph("Body text"),
        {},
        "",
        relationship("header", `${space}/word/document.xml`),
      ),
    );
    expect(result.part("word/document.xml")).toContain("Body text");
  });

  it("keeps a document with an embedded SVG image", async () => {
    const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>';
    const body = `${paragraph("Before")}<w:p><w:r><w:drawing><a:blip xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" r:embed="image"/></w:drawing></w:r></w:p>`;
    const result = await prepared(
      await wordFile(body, {}, "", relationship("image", "media/image1.svg"), {
        "word/media/image1.svg": svg,
      }),
    );
    expect(result.truncated).toBe(false);
    expect(await result.part("word/document.xml")).toContain("Before");
    expect(await result.part("word/media/image1.svg")).toBe(svg);
  });

  it("keeps a VML picture whose imagedata carries a shape id before r:id", async () => {
    const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>';
    const body = `<w:p><w:r><w:pict><v:shape><v:imagedata id="_x0000_i1025" r:id="image"/></v:shape></w:pict></w:r></w:p>`;
    const result = await prepared(
      await wordFile(body, {}, "", relationship("image", "media/image1.svg"), {
        "word/media/image1.svg": svg,
      }),
    );
    expect(await result.part("word/media/image1.svg")).toBe(svg);
  });
});

function workbookFile(
  target: string,
  sheetPath: string,
  extraRels = "",
  targetMode = "Internal",
  workbookPath = "xl/workbook.xml",
  files: Record<string, string> = {},
) {
  const slash = workbookPath.lastIndexOf("/") + 1;
  return zipFixture({
    "[Content_Types].xml": `<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/${workbookPath}" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>`,
    "_rels/.rels": `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">${relationship("officeDocument", workbookPath)}</Relationships>`,
    [workbookPath]: `<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" ${R}><sheets><sheet name="Sample" sheetId="1" r:id="sheet"/></sheets></workbook>`,
    [`${workbookPath.slice(0, slash)}_rels/${workbookPath.slice(slash)}.rels`]: `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheet" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="${target}" TargetMode="${targetMode}"/>${extraRels}</Relationships>`,
    [sheetPath]: `<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>42</v></c></row></sheetData></worksheet>`,
    ...files,
  }).buffer;
}

describe("Workbook relationship targets", () => {
  const prepare = async (bytes: ArrayBuffer) => {
    const result = await prepareOffice(bytes, "xlsx");
    return { truncated: result.truncated, part: zipEntries(result.bytes).text };
  };

  it.each(["sheet 1.xml", "sheet%201.xml", "sheet#1.xml"])(
    "prepares a worksheet whose ZIP name matches its target literally (%s)",
    async (name) => {
      const result = await prepare(workbookFile(`worksheets/${name}`, `xl/worksheets/${name}`));
      expect(result.truncated).toBe(false);
      expect(result.part(`xl/worksheets/${name}`)).toContain("42");
    },
  );

  it.each<[string, string, string, Record<string, string>]>([
    ["a worksheet whose relationship is external", "workbook-sheet-count", "External", {}],
    [
      "package relationships with a DOCTYPE",
      "office-xml-declarations",
      "Internal",
      { "_rels/.rels": "<!DOCTYPE Relationships><Relationships/>" },
    ],
  ])("refuses %s", async (_, code, targetMode, files) => {
    await expect(
      prepare(
        workbookFile(
          "worksheets/sheet2.xml",
          "xl/worksheets/sheet2.xml",
          "",
          targetMode,
          "xl/workbook.xml",
          files,
        ),
      ),
    ).rejects.toMatchObject({ code });
  });

  it.each<[string, string, string, Record<string, string>]>([
    ["workbook.xml", "worksheets/sheet1.xml", "worksheets/sheet1.xml", {}],
    ["book/main.xml", "../sheets/a&amp;b.xml", "sheets/a&b.xml", {}],
    ["book/main", "/sheets/sheet1.xml", "sheets/sheet1.xml", {}],
    [
      "xl/workbook2.xml",
      "worksheets/sheet1.xml",
      "xl/worksheets/sheet1.xml",
      {
        "xl/workbook.xml": "<workbook><sheets/></workbook>",
        "xl/_rels/workbook.xml.rels": "<Relationships/>",
      },
    ],
  ])(
    "moves a workbook found through package relationships (%s) to the engine's path",
    async (workbookPath, target, sheetPath, files) => {
      const result = await prepare(
        workbookFile(target, sheetPath, "", "Internal", workbookPath, files),
      );
      expect(result.part("xl/workbook.xml")).toContain('name="Sample"');
      expect(result.part("xl/_rels/workbook.xml.rels")).toContain(
        `Target="/${sheetPath.replace("&", "&amp;")}"`,
      );
      expect(result.part(sheetPath)).toContain("42");
    },
  );

  it("handles a malformed external relationship in workbook relationships", async () => {
    const result = await prepare(
      workbookFile(
        "worksheets/sheet1.xml",
        "xl/worksheets/sheet1.xml",
        `<Relationship Id="ext" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="http://[placeholder]/" TargetMode="External"/>`,
      ),
    );
    expect(result.truncated).toBe(false);
    expect(result.part("xl/worksheets/sheet1.xml")).toContain("42");
  });
});

describe("Workbook preview limits", () => {
  it("does not split a surrogate pair when truncating cell text", async () => {
    const prefix = "a".repeat(4095);
    const result = await prepareOffice(
      await workbookFile(
        "worksheets/sheet1.xml",
        "xl/worksheets/sheet1.xml",
        "",
        "Internal",
        "xl/workbook.xml",
        {
          "xl/worksheets/sheet1.xml": `<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>${prefix}🚀tail</t></is></c></row></sheetData></worksheet>`,
        },
      ),
      "xlsx",
    );
    const sheet = zipEntries(result.bytes).text("xl/worksheets/sheet1.xml");
    expect(result.truncated).toBe(true);
    expect(sheet).toContain(`<t>${prefix}</t>`);
    expect(sheet).not.toContain("\ufffd");
  });
});

describe("Case-insensitive XML size limits", () => {
  it("rejects uppercase .XML entries exceeding 8 MiB in office archives", async () => {
    const bytes = zipFixture(
      { "ppt/slides/slide1.XML": new Uint8Array(8 * 1024 * 1024 + 1) },
      { deflate: true },
    );
    await expect(prepareOffice(bytes.buffer, "pptx")).rejects.toMatchObject({
      code: "office-too-large-expanded",
    });
  });

  it("rejects uppercase .XML entries exceeding 8 MiB in encrypted OpenDocument manifests", async () => {
    const manifestXml =
      `<?xml version="1.0" encoding="UTF-8"?>` +
      `<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">` +
      `<manifest:file-entry manifest:full-path="content.XML" manifest:size="${8 * 1024 * 1024 + 1}">` +
      `<manifest:encryption-data manifest:checksum-type="SHA1/1K" manifest:checksum="AAAAAAAAAAAAAAAAAAAAAAAAAAA=">` +
      `<manifest:algorithm manifest:algorithm-name="http://www.w3.org/2001/04/xmlenc#aes256-cbc" manifest:initialisation-vector="AAAAAAAAAAAAAAAAAAAAAA=="/>` +
      `<manifest:key-derivation manifest:key-derivation-name="PBKDF2" manifest:iteration-count="1" manifest:salt="AAAAAAAAAAAAAAAAAAAAAA=="/>` +
      `</manifest:encryption-data>` +
      `</manifest:file-entry>` +
      `</manifest:manifest>`;
    const archive = new Map([["META-INF/manifest.xml", new TextEncoder().encode(manifestXml)]]);
    await expect(unlockOpenDocument(archive, "open me")).rejects.toMatchObject({
      code: "office-too-large-expanded",
    });
  });
});
