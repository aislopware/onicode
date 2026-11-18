import { observeResize } from "~/lib/observeResize";

import { skyEventsAt, skyWeatherAt } from "./sky";
import {
  paintSkyBackdrop,
  paintSkyLife,
  SKY_HEIGHT,
  skyControlColors,
  type SkyScene,
} from "./skyPaint";

const hourOf = (date: Date) => date.getHours() + date.getMinutes() / 60 + date.getSeconds() / 3600;

/**
 * Keeps a canvas painted with the sky for the current time. The backdrop is repainted once a
 * minute; the moving layer runs at two frames a second and faster only while an event needs it.
 * Nothing is painted while the canvas is hidden, and reduced motion leaves one frame a minute.
 * `controlsHost` receives the colours for the controls drawn over the scene.
 */
export function runSky(canvas: HTMLCanvasElement, controlsHost: HTMLElement): () => void {
  const painter = canvas.getContext("2d");
  const backdrop = document.createElement("canvas");
  const backdropPainter = backdrop.getContext("2d", { willReadFrequently: true });
  if (!painter || !backdropPainter) return () => {};

  const root = canvas.ownerDocument.documentElement;
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  let timer: number | undefined;
  let onScreen = true;
  let backdropKey = "";
  let controlColors: Record<string, string> = {};

  const paint = () => {
    timer = undefined;
    const width = canvas.offsetWidth;
    if (width === 0 || !onScreen || document.hidden) return;

    const now = new Date();
    const still = reducedMotion.matches;
    const minute = Math.floor(now.getTime() / 60_000);
    const scale = window.devicePixelRatio || 1;
    const scene: SkyScene = {
      width,
      dark: root.classList.contains("dark"),
      hour: hourOf(now),
      epochMs: still ? minute * 60_000 : now.getTime(),
      weather: skyWeatherAt(now),
    };

    const key = `${width}|${scale}|${scene.dark}|${minute}`;
    if (key !== backdropKey) {
      backdropKey = key;
      for (const surface of [canvas, backdrop]) {
        surface.width = Math.round(width * scale);
        surface.height = Math.round(SKY_HEIGHT * scale);
      }
      backdropPainter.setTransform(scale, 0, 0, scale, 0, 0);
      paintSkyBackdrop(backdropPainter, scene);
      controlColors = skyControlColors(scene);
      for (const [name, color] of Object.entries(controlColors)) {
        controlsHost.style.setProperty(name, color);
      }
    }

    painter.setTransform(scale, 0, 0, scale, 0, 0);
    painter.clearRect(0, 0, width, SKY_HEIGHT);
    painter.drawImage(backdrop, 0, 0, width, SKY_HEIGHT);
    const framesPerSecond = paintSkyLife(
      painter,
      scene,
      still ? [] : skyEventsAt(scene.epochMs / 1000),
    );
    const wait = still ? (minute + 1) * 60_000 - now.getTime() : 1000 / framesPerSecond;
    timer = window.setTimeout(paint, wait);
  };

  const repaint = () => {
    window.clearTimeout(timer);
    backdropKey = "";
    paint();
  };

  const stopObservingSize = observeResize(canvas, repaint);
  const themeObserver = new MutationObserver(repaint);
  themeObserver.observe(root, { attributes: true, attributeFilter: ["class"] });
  const screenObserver = new IntersectionObserver((entries) => {
    onScreen = entries.at(-1)?.isIntersecting ?? true;
    if (onScreen && timer === undefined) paint();
  });
  screenObserver.observe(canvas);
  const resume = () => {
    if (timer === undefined) paint();
  };
  document.addEventListener("visibilitychange", resume);
  reducedMotion.addEventListener("change", repaint);

  return () => {
    window.clearTimeout(timer);
    stopObservingSize();
    themeObserver.disconnect();
    screenObserver.disconnect();
    document.removeEventListener("visibilitychange", resume);
    reducedMotion.removeEventListener("change", repaint);
    for (const name of Object.keys(controlColors)) controlsHost.style.removeProperty(name);
  };
}
