import { expect, it, vi } from "vite-plus/test";
import { preserveReadingPosition } from "./preserveReadingPosition";

const nextEvent = (target: EventTarget, type: string) =>
  new Promise<void>((resolve) => target.addEventListener(type, () => resolve(), { once: true }));

it("keeps the caret line when an editor that became editable after mounting narrows", async () => {
  const editor = document.createElement("div");
  editor.style.cssText = "width:360px;height:200px;overflow-y:auto;font:16px/20px sans-serif";
  editor.textContent = Array.from(
    { length: 200 },
    (_, index) => `Sentence ${index + 1} keeps wrapping while the side panel changes width.`,
  ).join(" ");
  document.body.append(editor);
  using _editor = { [Symbol.dispose]: () => editor.remove() };
  using _preserve = { [Symbol.dispose]: preserveReadingPosition(editor) };

  editor.contentEditable = "true";
  editor.focus();
  const scrollTo = async (top: number) => {
    const scrolled = nextEvent(editor, "scroll");
    editor.scrollTop = top;
    await scrolled;
  };
  // The second scroll comes after the first frame's resize observation, so only the caret's own move
  // can capture it below.
  await scrollTo(600);
  await scrollTo(620);

  const box = editor.getBoundingClientRect();
  const point = document.caretPositionFromPoint(box.left + box.width / 2, box.top + 150)!;
  const moved = nextEvent(document, "selectionchange");
  document.getSelection()!.collapse(point.offsetNode, point.offset);
  await moved;
  const caretTop = () => {
    const range = document.createRange();
    range.setStart(point.offsetNode, point.offset);
    return range.getBoundingClientRect().top - editor.getBoundingClientRect().top;
  };
  const before = caretTop();

  editor.style.width = "240px";
  await vi.waitFor(() => expect(Math.abs(caretTop() - before)).toBeLessThan(2));
});
