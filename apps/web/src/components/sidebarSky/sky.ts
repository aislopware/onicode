/** A colour as OKLCH lightness, chroma and hue. Hues past 360 keep neighbouring keys close. */
export type SkyColor = readonly [lightness: number, chroma: number, hue: number];

export interface SkyPalette {
  /** Five stops from the top of the sky down to the horizon. */
  readonly sky: readonly SkyColor[];
  readonly farRidge: SkyColor;
  /** How dark the sky is, from 0 in daylight to 1 at night. */
  readonly night: number;
}

interface SkyKey extends SkyPalette {
  readonly hour: number;
}

const NIGHT_KEY: SkyPalette = {
  sky: [
    [0.13, 0.05, 272],
    [0.16, 0.07, 270],
    [0.2, 0.08, 266],
    [0.25, 0.09, 260],
    [0.3, 0.09, 255],
  ],
  farRidge: [0.17, 0.06, 270],
  night: 1,
};

const SKY_KEYS: readonly SkyKey[] = [
  { hour: 0, ...NIGHT_KEY },
  { hour: 4.5, ...NIGHT_KEY },
  {
    hour: 5.5,
    sky: [
      [0.17, 0.07, 275],
      [0.26, 0.11, 290],
      [0.42, 0.15, 335],
      [0.62, 0.16, 385],
      [0.76, 0.14, 420],
    ],
    farRidge: [0.22, 0.09, 295],
    night: 0.6,
  },
  {
    hour: 6.5,
    sky: [
      [0.34, 0.12, 268],
      [0.48, 0.13, 295],
      [0.64, 0.14, 365],
      [0.78, 0.14, 410],
      [0.88, 0.11, 440],
    ],
    farRidge: [0.3, 0.1, 290],
    night: 0.1,
  },
  {
    hour: 8.5,
    sky: [
      [0.4, 0.16, 258],
      [0.48, 0.16, 252],
      [0.58, 0.14, 245],
      [0.7, 0.1, 235],
      [0.8, 0.06, 225],
    ],
    farRidge: [0.34, 0.11, 262],
    night: 0,
  },
  {
    hour: 13,
    sky: [
      [0.42, 0.18, 260],
      [0.5, 0.17, 254],
      [0.6, 0.15, 246],
      [0.72, 0.1, 236],
      [0.82, 0.06, 226],
    ],
    farRidge: [0.36, 0.12, 260],
    night: 0,
  },
  {
    hour: 16.5,
    sky: [
      [0.4, 0.16, 262],
      [0.48, 0.15, 264],
      [0.6, 0.12, 280],
      [0.75, 0.1, 410],
      [0.87, 0.1, 445],
    ],
    farRidge: [0.33, 0.11, 270],
    night: 0,
  },
  {
    hour: 18,
    sky: [
      [0.3, 0.12, 272],
      [0.4, 0.14, 295],
      [0.56, 0.16, 350],
      [0.72, 0.16, 395],
      [0.84, 0.13, 430],
    ],
    farRidge: [0.27, 0.1, 295],
    night: 0.15,
  },
  {
    hour: 19,
    sky: [
      [0.2, 0.09, 280],
      [0.3, 0.13, 300],
      [0.45, 0.17, 345],
      [0.66, 0.17, 390],
      [0.8, 0.14, 425],
    ],
    farRidge: [0.24, 0.1, 300],
    night: 0.7,
  },
  {
    hour: 20.5,
    sky: [
      [0.15, 0.06, 276],
      [0.2, 0.09, 282],
      [0.27, 0.11, 298],
      [0.36, 0.13, 325],
      [0.45, 0.13, 345],
    ],
    farRidge: [0.19, 0.07, 285],
    night: 1,
  },
  { hour: 22, ...NIGHT_KEY },
  { hour: 24, ...NIGHT_KEY },
];

const NIGHT_KEY_AT_MIDNIGHT: SkyKey = { hour: 0, ...NIGHT_KEY };

const mixColor = (from: SkyColor, to: SkyColor, amount: number): SkyColor => [
  from[0] + (to[0] - from[0]) * amount,
  from[1] + (to[1] - from[1]) * amount,
  from[2] + (to[2] - from[2]) * amount,
];

const wrapHour = (hour: number) => ((hour % 24) + 24) % 24;

/** The sky's colours at a local hour, eased between the keys around it. */
export function skyPaletteAt(hour: number): SkyPalette {
  const wrapped = wrapHour(hour);
  const next = SKY_KEYS.findIndex((key) => key.hour > wrapped);
  const from = SKY_KEYS[next - 1] ?? NIGHT_KEY_AT_MIDNIGHT;
  const to = SKY_KEYS[next] ?? NIGHT_KEY_AT_MIDNIGHT;
  const linear = to.hour === from.hour ? 0 : (wrapped - from.hour) / (to.hour - from.hour);
  const eased = linear * linear * (3 - 2 * linear);
  return {
    sky: from.sky.map((color, index) => mixColor(color, to.sky[index] ?? color, eased)),
    farRidge: mixColor(from.farRidge, to.farRidge, eased),
    night: from.night + (to.night - from.night) * eased,
  };
}

const SUNRISE_HOUR = 6;
const SUNSET_HOUR = 18.5;

/** Which body is up, and how far along its crossing it is, from 0 at rising to 1 at setting. */
export function skyBodyAt(hour: number): { readonly sun: boolean; readonly progress: number } {
  const wrapped = wrapHour(hour);
  if (wrapped >= SUNRISE_HOUR && wrapped <= SUNSET_HOUR) {
    return { sun: true, progress: (wrapped - SUNRISE_HOUR) / (SUNSET_HOUR - SUNRISE_HOUR) };
  }
  const sinceSunset = (wrapped - SUNSET_HOUR + 24) % 24;
  return { sun: false, progress: sinceSunset / (24 - SUNSET_HOUR + SUNRISE_HOUR) };
}

const SYNODIC_MONTH_DAYS = 29.530588;
const KNOWN_NEW_MOON_MS = Date.UTC(2000, 0, 6, 18, 14);

/** The moon's phase: 0 new, 0.5 full, back to new at 1. */
export function moonPhaseAt(epochMs: number): number {
  const months = (epochMs - KNOWN_NEW_MOON_MS) / 86_400_000 / SYNODIC_MONTH_DAYS;
  return months - Math.floor(months);
}

/** A small seeded generator (mulberry32), so a day or an event always draws the same way. */
export function seededRandom(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let mixed = state;
    mixed = Math.imul(mixed ^ (mixed >>> 15), mixed | 1);
    mixed ^= mixed + Math.imul(mixed ^ (mixed >>> 7), mixed | 61);
    return ((mixed ^ (mixed >>> 14)) >>> 0) / 4_294_967_296;
  };
}

const SKY_EVENT_KINDS = [
  "flock",
  "wind",
  "balloon",
  "meteor",
  "satellite",
  "fireflies",
  "petals",
  "leaves",
  "snow",
  "aurora",
  "lanterns",
  "fireworks",
] as const;
export type SkyEventKind = (typeof SKY_EVENT_KINDS)[number];

export interface SkyEvent {
  readonly kind: SkyEventKind;
  /** Epoch seconds. */
  readonly startedAt: number;
  readonly seed: number;
}

/** What one local day is like: its cloud cover and its wind, in pixels a second, signed. */
export interface SkyWeather {
  readonly seed: number;
  readonly clouds: number;
  readonly wind: number;
}

const localDayNumber = (date: Date) =>
  Math.floor((date.getTime() - date.getTimezoneOffset() * 60_000) / 86_400_000);

export function skyWeatherAt(date: Date): SkyWeather {
  const seed = localDayNumber(date);
  const random = seededRandom(seed);
  return {
    seed,
    clouds: Math.floor(random() * 6),
    wind: (random() < 0.5 ? -1 : 1) * (0.5 + random() * 2.2),
  };
}

const LUNAR_CALENDAR = (() => {
  try {
    return new Intl.DateTimeFormat("en-u-ca-chinese", { month: "numeric", day: "numeric" });
  } catch {
    return null;
  }
})();

function lunarDateOf(date: Date): { readonly month: number; readonly day: number } | null {
  const parts = LUNAR_CALENDAR?.formatToParts(date) ?? [];
  const month = parts.find((part) => part.type === "month")?.value ?? "";
  const day = Number(parts.find((part) => part.type === "day")?.value);
  // A leap month is written with a suffix ("6bis") and is not a festival month.
  if (!/^\d+$/.test(month) || !Number.isInteger(day)) return null;
  return { month: Number(month), day };
}

interface SkyMoment {
  readonly hour: number;
  readonly month: number;
  readonly night: number;
  readonly wind: number;
  readonly newYear: boolean;
  readonly lanternNight: boolean;
  readonly auroraNight: boolean;
}

function skyMomentAt(epochSeconds: number): SkyMoment {
  const date = new Date(epochSeconds * 1000);
  const hour = date.getHours() + date.getMinutes() / 60 + date.getSeconds() / 3600;
  const lunar = lunarDateOf(date);
  const firstOfYear =
    (date.getMonth() === 0 && date.getDate() === 1) || (lunar?.month === 1 && lunar.day === 1);
  // One night runs past midnight, so it is named after the day it began on.
  const nightNumber = localDayNumber(new Date(date.getTime() - 12 * 3_600_000));
  return {
    hour,
    month: date.getMonth(),
    night: skyPaletteAt(hour).night,
    wind: skyWeatherAt(date).wind,
    newYear: firstOfYear && hour < 0.25,
    lanternNight:
      lunar?.day === 15 && (lunar.month === 1 || lunar.month === 8) && hour >= 19 && hour < 23,
    auroraNight: (hour >= 22 || hour < 3) && seededRandom(nightNumber * 31 + 7)() < 0.25,
  };
}

const inMonths = (moment: SkyMoment, first: number, last: number) =>
  moment.month >= first && moment.month <= last;

interface SkyEventRule {
  /** Seconds in one slot. A slot holds at most one event of this kind. */
  readonly every: number;
  readonly lasts: number;
  /** The chance that a slot starting at this moment holds the event. */
  readonly chance: (moment: SkyMoment) => number;
}

const SKY_EVENT_RULES: Record<SkyEventKind, SkyEventRule> = {
  flock: {
    every: 180,
    lasts: 16,
    chance: (moment) => (moment.night < 0.05 ? 0.4 : moment.night < 0.9 ? 0.9 : 0),
  },
  wind: {
    every: 150,
    lasts: 2.8,
    chance: (moment) => Math.abs(moment.wind) * (moment.night < 0.5 ? 0.25 : 0.08),
  },
  balloon: { every: 14_400, lasts: 90, chance: (moment) => (moment.night < 0.05 ? 0.5 : 0) },
  meteor: { every: 600, lasts: 0.9, chance: (moment) => (moment.night > 0.9 ? 0.7 : 0) },
  satellite: {
    every: 3600,
    lasts: 26,
    chance: (moment) =>
      moment.night > 0.5 &&
      ((moment.hour >= 19.5 && moment.hour < 21.5) || (moment.hour >= 4 && moment.hour < 5.5))
        ? 0.7
        : 0,
  },
  fireflies: {
    every: 900,
    lasts: 40,
    chance: (moment) =>
      inMonths(moment, 4, 7) && moment.hour >= 18.5 && moment.hour < 22.5 ? 0.7 : 0,
  },
  petals: {
    every: 240,
    lasts: 16,
    chance: (moment) => (inMonths(moment, 2, 3) && moment.night < 0.5 ? 0.6 : 0),
  },
  leaves: {
    every: 240,
    lasts: 16,
    chance: (moment) => (inMonths(moment, 8, 10) && moment.night < 0.5 ? 0.6 : 0),
  },
  snow: {
    every: 900,
    lasts: 40,
    chance: (moment) => (moment.month === 11 || moment.month <= 1 ? 0.5 : 0),
  },
  aurora: { every: 1800, lasts: 60, chance: (moment) => (moment.auroraNight ? 0.6 : 0) },
  lanterns: { every: 75, lasts: 60, chance: (moment) => (moment.lanternNight ? 1 : 0) },
  fireworks: { every: 12, lasts: 9, chance: (moment) => (moment.newYear ? 1 : 0) },
};

export const skyEventSeconds = (kind: SkyEventKind) => SKY_EVENT_RULES[kind].lasts;

/**
 * The events in the air at a time. Each kind's timeline is cut into slots, and a slot's number
 * alone decides whether it holds an event and when it starts, so nothing has to be remembered.
 */
export function skyEventsAt(epochSeconds: number): SkyEvent[] {
  const events: SkyEvent[] = [];
  SKY_EVENT_KINDS.forEach((kind, kindIndex) => {
    const rule = SKY_EVENT_RULES[kind];
    const slot = Math.floor(epochSeconds / rule.every);
    const random = seededRandom(Math.imul(slot, 2_654_435_761) + kindIndex * 7919);
    const startedAt = slot * rule.every + random() * (rule.every - rule.lasts);
    const roll = random();
    if (epochSeconds < startedAt || epochSeconds > startedAt + rule.lasts) return;
    if (roll >= rule.chance(skyMomentAt(startedAt))) return;
    events.push({ kind, startedAt, seed: Math.floor(random() * 1_000_000) });
  });
  return events;
}
