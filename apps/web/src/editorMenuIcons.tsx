import type { EditorId } from "@onicode/contracts";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";

import { type EditorIconOption, editorIconOptions } from "./components/editorIcons";

const ICON_PIXELS = 32;
const SVG_NS = "http://www.w3.org/2000/svg";

const iconClassName = (option: EditorIconOption) =>
  option.kind === "brand" ? "size-4 text-foreground" : "size-4 text-muted-foreground";

// Logos are styled by Tailwind and the theme, so render them in the document and inline the
// computed paint before rasterizing; native menus only accept bitmaps.
async function rasterizeEditorIcon(option: EditorIconOption): Promise<string | null> {
  const host = document.createElement("div");
  host.setAttribute("aria-hidden", "true");
  host.style.cssText = "position:fixed;left:-10000px;top:0;width:16px;height:16px;";
  document.body.append(host);
  const root = createRoot(host);
  try {
    flushSync(() => root.render(<option.Icon className={iconClassName(option)} />));
    const svg = host.querySelector("svg");
    if (svg === null) return null;
    const clone = svg.cloneNode(true) as SVGSVGElement;
    const sources = [svg, ...svg.querySelectorAll("*")];
    const targets = [clone, ...clone.querySelectorAll("*")];
    sources.forEach((source, index) => {
      const computed = getComputedStyle(source);
      const target = targets[index];
      target?.setAttribute(
        "style",
        `${target.getAttribute("style") ?? ""};fill:${computed.fill};stroke:${computed.stroke}`,
      );
    });
    clone.removeAttribute("class");
    clone.setAttribute("xmlns", SVG_NS);
    clone.setAttribute("width", String(ICON_PIXELS));
    clone.setAttribute("height", String(ICON_PIXELS));
    const image = new Image();
    image.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(new XMLSerializer().serializeToString(clone))}`;
    await image.decode();
    const canvas = document.createElement("canvas");
    canvas.width = ICON_PIXELS;
    canvas.height = ICON_PIXELS;
    const context = canvas.getContext("2d");
    if (context === null) return null;
    context.drawImage(image, 0, 0, ICON_PIXELS, ICON_PIXELS);
    return canvas.toDataURL("image/png");
  } catch {
    return null;
  } finally {
    root.unmount();
    host.remove();
  }
}

/** What a logo's pixels depend on: `dark:` fills and the theme color behind currentColor. */
function paintKey(className: string): string {
  const probe = document.createElement("span");
  probe.className = className;
  document.body.append(probe);
  const { color } = getComputedStyle(probe);
  probe.remove();
  return `${document.documentElement.classList.contains("dark")}|${color}`;
}

const rasterized = new Map<
  EditorId,
  { readonly paint: string; readonly icon: Promise<string | null> }
>();

/** PNG data URLs of editor logos for context menus, rasterized on first use per theme paint. */
export async function editorMenuIcons(
  editors: ReadonlyArray<EditorId>,
): Promise<ReadonlyMap<EditorId, string>> {
  const options = editorIconOptions(navigator.platform);
  const paintByClassName = new Map<string, string>();
  const icons = await Promise.all(
    editors.map(async (editor) => {
      const option = options.find(({ value }) => value === editor);
      if (option === undefined) return null;
      const className = iconClassName(option);
      const paint = paintByClassName.get(className) ?? paintKey(className);
      paintByClassName.set(className, paint);
      let entry = rasterized.get(editor);
      if (entry?.paint !== paint) {
        entry = { paint, icon: rasterizeEditorIcon(option) };
        rasterized.set(editor, entry);
      }
      const icon = await entry.icon;
      // A failed render may be transient; try again on the next menu open.
      if (icon === null && rasterized.get(editor) === entry) rasterized.delete(editor);
      return icon === null ? null : ([editor, icon] as const);
    }),
  );
  return new Map(icons.filter((entry) => entry !== null));
}
