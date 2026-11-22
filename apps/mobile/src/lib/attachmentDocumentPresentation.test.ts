import { expect, it } from "vite-plus/test";
import { filePreviewKind } from "@onicode/shared/filePreview";

it.each([
  [
    "report.docx",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "unsupported",
  ],
  [
    "budget.xlsx",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "unsupported",
  ],
  [
    "deck.pptx",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "unsupported",
  ],
  ["report.docx", "text/plain", "text"],
  ["report.docx", "application/octet-stream", "unsupported"],
  ["chart.mmd", "text/vnd.mermaid", "text"],
  ["chart.mermaid", "text/plain", "text"],
  ["chart.mmd", "application/octet-stream", "unsupported"],
  ["font.ttf", "font/ttf", "unsupported"],
  // No mobile renderer for RTF: the OS viewer opens it rather than raw markup.
  ["notes.rtf", "text/rtf", "unsupported"],
  ["notes.rtf", "text/plain", "unsupported"],
  ["download", "text/rtf; charset=us-ascii", "unsupported"],
] as const)("preserves mobile's existing path for %s (%s)", (name, mimeType, expected) => {
  expect(filePreviewKind({ name, mimeType }, { richDocuments: false })).toBe(expected);
});
