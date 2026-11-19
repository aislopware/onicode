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

/** How much of each season the date holds. The four add up to 1. */
export interface SkySeason {
  readonly spring: number;
  readonly summer: number;
  readonly autumn: number;
  readonly winter: number;
}

const YEAR_DAYS = 365.2425;
// The days an alpine valley looks most like its season: deep snow, first meadow, high summer, larches.
const SEASON_PEAK_DAYS = { winter: 15, spring: 108, summer: 200, autumn: 292 } as const;
const SEASON_ORDER = ["winter", "spring", "summer", "autumn"] as const;
type SeasonName = (typeof SEASON_ORDER)[number];

const smooth = (amount: number) => {
  const clamped = Math.min(1, Math.max(0, amount));
  return clamped * clamped * (3 - 2 * clamped);
};

/** South of the equator the year runs half a turn ahead, as far as the time zone can tell. */
const SOUTHERN_ZONE =
  /^(Australia|Antarctica)\/|^Pacific\/(Auckland|Chatham|Fiji|Tongatapu|Apia|Noumea|Port_Moresby|Efate|Guadalcanal|Rarotonga|Tahiti|Easter)$|^America\/(Sao_Paulo|Argentina\/|Buenos_Aires|Santiago|Punta_Arenas|Montevideo|Asuncion|La_Paz|Lima|Campo_Grande|Cuiaba|Porto_Velho|Rio_Branco|Bahia|Maceio|Recife|Araguaina)|^Africa\/(Johannesburg|Maputo|Windhoek|Gaborone|Harare|Lusaka|Maseru|Mbabane|Blantyre|Luanda|Lubumbashi|Dar_es_Salaam)$|^Indian\/(Antananarivo|Mauritius|Reunion)$|^Atlantic\/Stanley$/;

export const isSouthernTimeZone = (timeZone: string) => SOUTHERN_ZONE.test(timeZone);

export function skySeasonAt(date: Date, southern = false): SkySeason {
  const startOfYear = new Date(date.getFullYear(), 0, 1).getTime();
  const dayOfYear = (date.getTime() - startOfYear) / 86_400_000;
  const day = (dayOfYear + (southern ? YEAR_DAYS / 2 : 0)) % YEAR_DAYS;
  const weights = { winter: 0, spring: 0, summer: 0, autumn: 0 };
  SEASON_ORDER.forEach((name, index) => {
    const next = SEASON_ORDER[(index + 1) % SEASON_ORDER.length] ?? name;
    const from = SEASON_PEAK_DAYS[name];
    const span = (SEASON_PEAK_DAYS[next] - from + YEAR_DAYS) % YEAR_DAYS;
    const since = (day - from + YEAR_DAYS) % YEAR_DAYS;
    if (since >= span) return;
    const into = smooth(since / span);
    weights[name] += 1 - into;
    weights[next] += into;
  });
  return weights;
}

/** The hours the sun rises and sets: long days in summer, short ones in winter. */
function skyDaylightAt(season: SkySeason): { readonly rise: number; readonly set: number } {
  return {
    rise: SUNRISE_HOUR - 0.6 * season.summer + 1.2 * season.winter,
    set: SUNSET_HOUR + 1.5 * season.summer - 1.5 * season.winter,
  };
}

/**
 * The hour the sky's keys are written for: the season's sunrise lands on the keys' sunrise and
 * its sunset on theirs, so a winter dusk comes early and a summer one late.
 */
export function skySolarHour(hour: number, season: SkySeason): number {
  const wrapped = wrapHour(hour);
  const { rise, set } = skyDaylightAt(season);
  if (wrapped < rise) return (wrapped / rise) * SUNRISE_HOUR;
  if (wrapped <= set) {
    return SUNRISE_HOUR + ((wrapped - rise) / (set - rise)) * (SUNSET_HOUR - SUNRISE_HOUR);
  }
  return SUNSET_HOUR + ((wrapped - set) / (24 - set)) * (24 - SUNSET_HOUR);
}

interface SeasonLook {
  /** What the top of the sky leans toward, and how far. */
  readonly high: SkyColor;
  readonly highAmount: number;
  /** The same for the sky at the horizon. */
  readonly low: SkyColor;
  readonly lowAmount: number;
  readonly ridge: SkyColor;
  readonly ridgeAmount: number;
  /** The valley floor in daylight: snow, meadow or larch gold. */
  readonly ground: SkyColor;
  /** How far down the far ridge its snow reaches, from 0 to 1. */
  readonly snow: number;
}

// The sky stays in the blues all year and the land carries the season, so no season turns muddy.
const SEASON_LOOKS: Record<SeasonName, SeasonLook> = {
  winter: {
    high: [0.5, 0.155, 250],
    highAmount: 0.45,
    low: [0.93, 0.035, 225],
    lowAmount: 0.5,
    ridge: [0.5, 0.075, 252],
    ridgeAmount: 0.6,
    ground: [0.95, 0.04, 240],
    snow: 1,
  },
  spring: {
    high: [0.52, 0.15, 245],
    highAmount: 0.35,
    low: [0.9, 0.055, 200],
    lowAmount: 0.45,
    ridge: [0.44, 0.09, 232],
    ridgeAmount: 0.5,
    ground: [0.62, 0.14, 138],
    snow: 0.5,
  },
  summer: {
    high: [0.43, 0.19, 262],
    highAmount: 0.3,
    low: [0.85, 0.07, 215],
    lowAmount: 0.3,
    ridge: [0.38, 0.11, 252],
    ridgeAmount: 0.4,
    ground: [0.47, 0.13, 148],
    snow: 0.2,
  },
  autumn: {
    high: [0.42, 0.17, 264],
    highAmount: 0.3,
    low: [0.9, 0.05, 75],
    lowAmount: 0.3,
    ridge: [0.42, 0.09, 285],
    ridgeAmount: 0.5,
    ground: [0.62, 0.17, 48],
    snow: 0.38,
  },
};

const RADIANS = Math.PI / 180;

/** Mixes in OKLab, where a blue leaning toward amber passes through grey, not through green. */
function tintColor(color: SkyColor, toward: SkyColor, amount: number): SkyColor {
  const fromA = color[1] * Math.cos(color[2] * RADIANS);
  const fromB = color[1] * Math.sin(color[2] * RADIANS);
  const a = fromA + (toward[1] * Math.cos(toward[2] * RADIANS) - fromA) * amount;
  const b = fromB + (toward[1] * Math.sin(toward[2] * RADIANS) - fromB) * amount;
  return [
    color[0] + (toward[0] - color[0]) * amount,
    Math.hypot(a, b),
    (Math.atan2(b, a) / RADIANS + 360) % 360,
  ];
}

/** Mixes around the hue wheel, as leaves turn: green goes to orange through gold, not through brown. */
function turnColor(color: SkyColor, toward: SkyColor, amount: number): SkyColor {
  const turn = ((toward[2] - color[2] + 540) % 360) - 180;
  // A wide turn passes through yellow, which only looks clean when it is light.
  const lift = 0.36 * Math.sin(Math.PI * amount) * (Math.abs(turn) / 180);
  return [
    color[0] + (toward[0] - color[0]) * amount + lift,
    color[1] + (toward[1] - color[1]) * amount,
    (color[2] + turn * amount + 360) % 360,
  ];
}

const NIGHT_LAND: SkyColor = [0.24, 0.07, 266];

/** How far down the far ridge the snow lies, from the summer peaks to the winter valley. */
export const skySnowAt = (season: SkySeason) =>
  SEASON_ORDER.reduce((snow, name) => snow + season[name] * SEASON_LOOKS[name].snow, 0);

// The land takes a little of the horizon's light, and at night sinks into the blue of the dark.
const landLit = (color: SkyColor, palette: SkyPalette) =>
  tintColor(tintColor(color, palette.sky[4] ?? color, 0.14), NIGHT_LAND, 0.66 * palette.night);

/** The valley floor under any snow, in the season's colour and the hour's light. */
export function skyGroundAt(season: SkySeason, palette: SkyPalette): SkyColor {
  let shared = 0;
  let ground: SkyColor = SEASON_LOOKS.spring.ground;
  for (const name of SEASON_ORDER) {
    // Winter lies on the land as snow instead of bleaching its colour to sand.
    if (name === "winter") continue;
    // Cubed, so the land keeps its season's colour until the next one is well under way.
    const share = season[name] ** 3;
    shared += share;
    if (shared > 0) ground = turnColor(ground, SEASON_LOOKS[name].ground, share / shared);
  }
  return landLit(ground, palette);
}

/** How much of the valley floor lies under snow, and the snow in the hour's light. */
export const skySnowfieldAt = (season: SkySeason, palette: SkyPalette) => ({
  cover: smooth((season.winter - 0.3) / 0.45),
  color: landLit(SEASON_LOOKS.winter.ground, palette),
});

/** A palette as the season colours it. Night keeps most of its own colour. */
export function seasonedPalette(palette: SkyPalette, season: SkySeason): SkyPalette {
  const daylight = 1 - 0.7 * palette.night;
  const tinted = (color: SkyColor, pick: (look: SeasonLook) => readonly [SkyColor, number]) =>
    SEASON_ORDER.reduce<SkyColor>((mixed, name) => {
      const [toward, amount] = pick(SEASON_LOOKS[name]);
      // Each season pulls by its share, so two seasons meeting blend instead of stacking.
      return tintColor(mixed, toward, amount * daylight * season[name]);
    }, color);
  // How far down the sky a stop sits: the horizon takes the low tint, the zenith the high one.
  const depth = [0, 0.2, 0.5, 0.85, 1];
  return {
    night: palette.night,
    sky: palette.sky.map((color, index) => {
      const down = depth[index] ?? 1;
      const high = tinted(color, (look) => [look.high, look.highAmount * (1 - down)]);
      // A sunset's own colour stays: the horizon tint eases off as the stop grows vivid and warm.
      const vivid = Math.min(1, Math.max(0, (color[1] - 0.11) / 0.05));
      return tinted(high, (look) => [look.low, look.lowAmount * down * (1 - 0.6 * vivid)]);
    }),
    farRidge: tinted(palette.farRidge, (look) => [look.ridge, look.ridgeAmount]),
  };
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

function skyMomentAt(epochSeconds: number, southern: boolean): SkyMoment {
  const date = new Date(epochSeconds * 1000);
  const hour = date.getHours() + date.getMinutes() / 60 + date.getSeconds() / 3600;
  const lunar = lunarDateOf(date);
  const firstOfYear =
    (date.getMonth() === 0 && date.getDate() === 1) || (lunar?.month === 1 && lunar.day === 1);
  // One night runs past midnight, so it is named after the day it began on.
  const nightNumber = localDayNumber(new Date(date.getTime() - 12 * 3_600_000));
  return {
    hour,
    // The petals, leaves and snow in the air keep to the land's season.
    month: (date.getMonth() + (southern ? 6 : 0)) % 12,
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
export function skyEventsAt(epochSeconds: number, southern = false): SkyEvent[] {
  const events: SkyEvent[] = [];
  SKY_EVENT_KINDS.forEach((kind, kindIndex) => {
    const rule = SKY_EVENT_RULES[kind];
    const slot = Math.floor(epochSeconds / rule.every);
    const random = seededRandom(Math.imul(slot, 2_654_435_761) + kindIndex * 7919);
    const startedAt = slot * rule.every + random() * (rule.every - rule.lasts);
    const roll = random();
    if (epochSeconds < startedAt || epochSeconds > startedAt + rule.lasts) return;
    if (roll >= rule.chance(skyMomentAt(startedAt, southern))) return;
    events.push({ kind, startedAt, seed: Math.floor(random() * 1_000_000) });
  });
  return events;
}
