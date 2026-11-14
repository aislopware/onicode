import type { GhosttyColor } from "./core";

function channelLuminance(channel: number): number {
  const value = channel / 255;
  return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

function luminance(r: number, g: number, b: number): number {
  return 0.2126 * channelLuminance(r) + 0.7152 * channelLuminance(g) + 0.0722 * channelLuminance(b);
}

function contrastRatio(left: number, right: number): number {
  return (Math.max(left, right) + 0.05) / (Math.min(left, right) + 0.05);
}

function shiftLuminance(
  color: GhosttyColor,
  backgroundLuminance: number,
  ratio: number,
  toward: 0 | 255,
): { readonly color: GhosttyColor; readonly ratio: number } {
  // Rounding toward the target never lowers contrast below the searched mix.
  const round = toward === 0 ? Math.floor : Math.ceil;
  const mix = (amount: number): GhosttyColor => ({
    r: round(color.r + (toward - color.r) * amount),
    g: round(color.g + (toward - color.g) * amount),
    b: round(color.b + (toward - color.b) * amount),
  });
  const ratioOf = (candidate: GhosttyColor) =>
    contrastRatio(luminance(candidate.r, candidate.g, candidate.b), backgroundLuminance);
  const extreme = mix(1);
  const extremeRatio = ratioOf(extreme);
  if (extremeRatio < ratio) return { color: extreme, ratio: extremeRatio };
  // Mixing toward black or white keeps the hue; search for the smallest mix that is legible.
  let low = 0;
  let high = 1;
  for (let step = 0; step < 10; step += 1) {
    const middle = (low + high) / 2;
    if (ratioOf(mix(middle)) >= ratio) high = middle;
    else low = middle;
  }
  const adjusted = mix(high);
  return { color: adjusted, ratio: ratioOf(adjusted) };
}

/** A background is dark when white text contrasts with it more than black text does. */
export function isDarkBackground(background: GhosttyColor): boolean {
  const backgroundLuminance = luminance(background.r, background.g, background.b);
  return contrastRatio(1, backgroundLuminance) > contrastRatio(0, backgroundLuminance);
}

/** Returns `foreground` moved toward black or white until it reaches `ratio` against `background`. */
export function ensureContrast(
  foreground: GhosttyColor,
  background: GhosttyColor,
  ratio: number,
): GhosttyColor {
  const backgroundLuminance = luminance(background.r, background.g, background.b);
  const foregroundLuminance = luminance(foreground.r, foreground.g, foreground.b);
  if (contrastRatio(foregroundLuminance, backgroundLuminance) >= ratio) return foreground;
  const [first, second] =
    foregroundLuminance < backgroundLuminance ? ([0, 255] as const) : ([255, 0] as const);
  const preferred = shiftLuminance(foreground, backgroundLuminance, ratio, first);
  if (preferred.ratio >= ratio) return preferred.color;
  const fallback = shiftLuminance(foreground, backgroundLuminance, ratio, second);
  return fallback.ratio > preferred.ratio ? fallback.color : preferred.color;
}

// Block elements, legacy-computing mosaics and powerline separators paint shapes that must match
// the neighbouring cell's background (Ghostty exempts the same ranges), so recolouring them would
// leave seams. Powerline's text-like icons below U+E0B0 stay subject to contrast.
export function isContrastExemptText(text: string): boolean {
  const code = text.codePointAt(0);
  if (code === undefined) return true;
  return (
    (code >= 0x2580 && code <= 0x259f) ||
    (code >= 0xe0b0 && code <= 0xe0d7) ||
    (code >= 0x1fb00 && code <= 0x1fbff) ||
    (code >= 0x1cc00 && code <= 0x1cebf)
  );
}

const CACHE_LIMIT = 4_096;

/** Memoizes `ensureContrast` per colour pair; terminals reuse a handful of pairs every frame. */
export class TerminalContrastCache {
  private readonly entries = new Map<number, GhosttyColor>();

  constructor(readonly ratio: number) {}

  adjust(foreground: GhosttyColor, background: GhosttyColor, faint: boolean): GhosttyColor {
    const key =
      ((foreground.r << 16) | (foreground.g << 8) | foreground.b) * 0x2000000 +
      ((background.r << 16) | (background.g << 8) | background.b) * 2 +
      (faint ? 1 : 0);
    const cached = this.entries.get(key);
    if (cached) return cached;
    // Faint text keeps a softer floor so it still reads as secondary.
    const adjusted = ensureContrast(foreground, background, faint ? this.ratio / 2 : this.ratio);
    if (this.entries.size >= CACHE_LIMIT) this.entries.clear();
    this.entries.set(key, adjusted);
    return adjusted;
  }
}
