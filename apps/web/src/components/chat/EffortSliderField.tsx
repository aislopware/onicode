import "culori/css";
import { converter, parse } from "culori/fn";
import { useEffect, useRef } from "react";

import { observeResize } from "~/lib/observeResize";

/**
 * The lit part of the effort slider: small cells from the left edge to the thumb, in hues taken
 * around the theme's accent. Each level has its own hue, and a level change sends a wave from the
 * thumb that carries the new hue. The top level keeps pulsing.
 */

const CELL_PITCH = 4;
const CELL_SIZE = 3;
const CELL_RADIUS = 0.9;
/** The thumb's half width: its centre runs this far inside the field at both ends. */
const THUMB_HALF = 10;
/** Hue, in degrees from the accent, at the fast end and at the smart end. */
const HUE_FAST = -50;
const HUE_SMART = 64;
/** How far the tone falls from the thumb back to the left edge. */
const TONE_SPREAD = 0.16;
const RAMP_STEPS = 64;
const WAVE_LIFE_MS = 2600;
const PULSE_EVERY_MS = 1500;
/** Cells light in whole steps, so waves repaint about 30 times a second, not at the display's rate. */
const WAVE_FRAME_MS = 24;
const STEPS = 5;

type Rgb = readonly [number, number, number];
type Wave = { at: number; x: number; strength: number; tone: number; seed: number };

const toOklch = converter("oklch");
const toRgb = converter("rgb");
/**
 * Where a level sits on the hue range: 0 is the fast end, 1 the smart end, and the default level
 * is 0.5, the accent itself, whenever it is not at an end.
 */
export function effortTone(index: number, last: number, defaultIndex: number): number {
  if (last <= 0) return 0.5;
  if (defaultIndex <= 0 || defaultIndex >= last) return Math.max(0, index) / last;
  if (index <= defaultIndex) return (0.5 * Math.max(0, index)) / defaultIndex;
  return 0.5 + (0.5 * (index - defaultIndex)) / (last - defaultIndex);
}

/** A tone's hue, in degrees from the accent. */
export function effortHue(tone: number): number {
  return tone < 0.5 ? HUE_FAST * (1 - 2 * tone) : HUE_SMART * (2 * tone - 1);
}

/** The accent as the browser resolved it, walked along the hue range: one color per tone step. */
function toneRamp(probe: Element, dark: boolean): Rgb[] {
  const accent = toOklch(parse(getComputedStyle(probe).color) ?? "#346bf1");
  const chroma = Math.min(0.21, Math.max(0.15, accent?.c ?? 0.2));
  const hue = accent?.h ?? 264;
  return Array.from({ length: RAMP_STEPS }, (_, step) => {
    const tone = step / (RAMP_STEPS - 1);
    const rgb = toRgb({
      mode: "oklch",
      l: dark ? 0.68 + 0.04 * tone : 0.58,
      c: chroma,
      h: hue + effortHue(tone),
    });
    const channel = (value: number) => Math.round(Math.min(1, Math.max(0, value)) * 255);
    return [channel(rgb.r), channel(rgb.g), channel(rgb.b)] as const;
  });
}

/** A stable pseudo-random number in [0, 1) for a cell, so the mosaic does not shimmer. */
function cellNoise(column: number, row: number, salt: number): number {
  const value = Math.sin(column * 127.1 + row * 311.7 + salt * 74.7) * 43758.5453;
  return value - Math.floor(value);
}

export function EffortSliderField(props: {
  /** The shown level and the top one, both as indexes. */
  level: number;
  top: number;
  /** The shown level's place on the hue range, from effortTone. */
  tone: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const engineRef = useRef<{
    setLevel: (level: number, top: number, tone: number) => void;
  } | null>(null);

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- drives the canvas: its frame loop, its size and its colors live outside React
  useEffect(() => {
    const canvas = canvasRef.current;
    const context = canvas?.getContext("2d");
    if (!canvas || !context) return;
    const still = window.matchMedia("(prefers-reduced-motion: reduce)");
    let width = 0;
    let height = 0;
    let ramp: Rgb[] = [];
    let dark = false;
    // The first setLevel places the field; later ones move it and send a wave.
    let placed = false;
    let level = 0;
    let top = 1;
    let tone = 0.5;
    // The tone of the cells no live wave has reached yet.
    let settledTone = 0.5;
    // Where the lit part ends, as a fraction. It glides to the level like the thumb does.
    let reach = 0;
    let waves: Wave[] = [];
    let lastPulse = 0;
    let frame = 0;
    let lastTime = 0;
    let lastDraw = 0;

    const thumbX = (fraction: number) => THUMB_HALF + fraction * (width - 2 * THUMB_HALF);

    const readTheme = () => {
      dark = canvas.closest(".dark") !== null;
      ramp = toneRamp(canvas, dark);
    };

    const measure = () => {
      const scale = window.devicePixelRatio || 1;
      width = canvas.clientWidth;
      height = canvas.clientHeight;
      canvas.width = Math.round(width * scale);
      canvas.height = Math.round(height * scale);
      context.setTransform(scale, 0, 0, scale, 0, 0);
      readTheme();
    };

    const draw = (now: number) => {
      context.clearRect(0, 0, width, height);
      const rows = Math.max(1, Math.round(height / CELL_PITCH));
      const pitch = height / rows;
      const size = (CELL_SIZE / CELL_PITCH) * pitch;
      const end = thumbX(reach);
      const strength = level / top;
      for (let column = 0; (column + 0.5) * pitch < end; column++) {
        const centerX = (column + 0.5) * pitch;
        const along = centerX / end;
        // The field deepens toward the thumb, and higher levels sit deeper overall.
        const depth = 0.42 + 0.58 * Math.pow(along, 0.8);
        for (let row = 0; row < rows; row++) {
          let energy = 0;
          let cellTone = settledTone;
          for (const wave of waves) {
            const age = (now - wave.at) / 1000;
            const distance = Math.abs(centerX - wave.x) / pitch + Math.abs(row - (rows - 1) / 2);
            const speed = 30 + 20 * wave.strength;
            const since = age - distance / speed - cellNoise(column, row, wave.seed) * 0.12;
            if (since < 0) continue;
            // The wave leaves its level's color on every cell it has passed.
            cellTone = wave.tone;
            // It lights only its own share of them, a larger share when it is stronger.
            if (cellNoise(column, row, wave.seed + 3.3) > 0.3 + 0.5 * wave.strength) continue;
            const head = Math.exp(-since * 9);
            const tail = 0.6 * Math.exp(-since * (3.2 - 2.3 * wave.strength));
            energy += (head + tail) * (0.5 + 0.5 * wave.strength);
          }
          // Warmest at the thumb, a little cooler behind it.
          const place = cellTone - TONE_SPREAD * Math.pow(1 - along, 1.5);
          const color = ramp[Math.round(Math.min(1, Math.max(0, place)) * (ramp.length - 1))];
          if (!color) continue;
          const personal = cellNoise(column, row, 8.8);
          const step = Math.min(
            STEPS,
            Math.floor(Math.min(1, energy * (0.4 + 0.6 * personal)) * STEPS + 0.2),
          );
          const mosaic = 0.68 + 0.32 * cellNoise(column, row, 1.7);
          const resting = (dark ? 0.94 : 0.7) * depth * mosaic * (0.72 + 0.28 * strength);
          // A lit cell climbs in whole steps, never as a smooth fade: to white on a dark
          // surface, to the full ink on a light one.
          const heat = step / STEPS;
          const alpha = Math.min(1, resting + (1 - resting) * heat);
          const toWhite = dark ? heat * 0.85 : 0;
          const red = Math.round(color[0] + (255 - color[0]) * toWhite);
          const green = Math.round(color[1] + (255 - color[1]) * toWhite);
          const blue = Math.round(color[2] + (255 - color[2]) * toWhite);
          context.fillStyle = `rgb(${red} ${green} ${blue} / ${alpha.toFixed(3)})`;
          context.beginPath();
          context.roundRect(
            centerX - size / 2,
            (row + 0.5) * pitch - size / 2,
            size,
            size,
            CELL_RADIUS,
          );
          context.fill();
        }
      }
    };

    const tick = (now: number) => {
      frame = 0;
      // The thumb's glide follows the display; waves alone skip the frames in between.
      if (reach === level / top && now - lastDraw < WAVE_FRAME_MS) {
        frame = requestAnimationFrame(tick);
        return;
      }
      lastDraw = now;
      const elapsed = Math.min(64, now - (lastTime || now));
      lastTime = now;
      const target = level / top;
      reach += (target - reach) * Math.min(1, elapsed / 60);
      if (Math.abs(target - reach) < 0.001) reach = target;
      // A wave has crossed the whole field long before it expires, so its color is settled.
      while (waves[0] && now - waves[0].at >= WAVE_LIFE_MS) {
        settledTone = waves[0].tone;
        waves = waves.slice(1);
      }
      // The top level is held, so it keeps sending waves like a press that never lets go.
      if (level === top && !still.matches && now - lastPulse > PULSE_EVERY_MS) {
        lastPulse = now;
        waves.push({ at: now, x: thumbX(target), strength: 1, tone, seed: now % 97 });
      }
      draw(now);
      const live = reach !== target || waves.length > 0 || (level === top && !still.matches);
      if (live) frame = requestAnimationFrame(tick);
      else lastTime = 0;
    };

    const wake = () => {
      if (frame === 0) frame = requestAnimationFrame(tick);
    };

    engineRef.current = {
      setLevel(nextLevel, nextTop, nextTone) {
        const changed = placed && nextLevel !== level;
        level = nextLevel;
        top = Math.max(1, nextTop);
        tone = nextTone;
        readTheme();
        if (!placed || still.matches) {
          placed = true;
          reach = level / top;
          settledTone = tone;
          waves = [];
        } else if (changed) {
          const now = performance.now();
          lastPulse = now;
          const dropped = waves.length > 7 ? waves[waves.length - 8] : undefined;
          if (dropped) settledTone = dropped.tone;
          waves = [
            ...waves.slice(-7),
            { at: now, x: thumbX(level / top), strength: level / top, tone, seed: now % 97 },
          ];
        }
        wake();
      },
    };

    const stopObserving = observeResize(canvas, () => {
      measure();
      wake();
    });
    measure();
    wake();
    return () => {
      engineRef.current = null;
      stopObserving();
      if (frame !== 0) cancelAnimationFrame(frame);
    };
    // The engine reads later levels through setLevel, so it is built once.
  }, []);

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- hands the level to the canvas engine, which sends a wave from the thumb
  useEffect(() => {
    engineRef.current?.setLevel(props.level, props.top, props.tone);
  }, [props.level, props.top, props.tone]);

  return (
    <span
      aria-hidden="true"
      className="pointer-events-none absolute inset-0.5 overflow-hidden rounded-md"
    >
      <canvas ref={canvasRef} className="block size-full text-primary" />
    </span>
  );
}
