import {
  moonPhaseAt,
  seasonedPalette,
  skyGroundAt,
  skySnowfieldAt,
  skySnowAt,
  seededRandom,
  skyBodyAt,
  skyEventSeconds,
  skyPaletteAt,
  skySolarHour,
  type SkyColor,
  type SkyEvent,
  type SkyPalette,
  type SkySeason,
  type SkyWeather,
} from "./sky";

export const SKY_HEIGHT = 104;
const HORIZON = 64;
const SUN_CLIMB = 16;
const BODY_RADIUS = 5;
const DOT_PITCH = 2.5;
const IDLE_FRAMES_PER_SECOND = 2;

export interface SkyScene {
  readonly width: number;
  readonly dark: boolean;
  readonly hour: number;
  readonly epochMs: number;
  readonly weather: SkyWeather;
  readonly season: SkySeason;
}

/** The hour on the sky's own clock, which the season stretches or shortens. */
const solarHourOf = (scene: SkyScene) => skySolarHour(scene.hour, scene.season);

const paletteOf = (scene: SkyScene): SkyPalette =>
  seasonedPalette(skyPaletteAt(solarHourOf(scene)), scene.season);

type Painter = CanvasRenderingContext2D;

const oklch = (lightness: number, chroma: number, hue: number, alpha = 1) =>
  `oklch(${lightness} ${chroma} ${hue} / ${alpha})`;

// On a light sidebar the whole picture is lifted, or a night sky would sit on it like a hole.
const lifted = (scene: SkyScene, lightness: number) =>
  scene.dark ? lightness : lightness * 0.6 + 0.33;

const paint = (scene: SkyScene, [lightness, chroma, hue]: SkyColor, alpha = 1) =>
  oklch(lifted(scene, lightness), chroma, hue, alpha);

const farRidgeAt = (x: number) =>
  HORIZON - 9 * Math.sin(x / 31 + 1.1) - 5 * Math.sin(x / 12.5 + 0.3) - 3 * Math.sin(x / 5.7);

const nearRidgeAt = (x: number) =>
  HORIZON + 12 - 7 * Math.sin(x / 44 + 4.2) - 3 * Math.sin(x / 15 + 1);

function traceRidge(painter: Painter, width: number, ridgeAt: (x: number) => number) {
  painter.beginPath();
  painter.moveTo(0, SKY_HEIGHT);
  for (let x = 0; x <= width; x++) painter.lineTo(x, ridgeAt(x));
  painter.lineTo(width, SKY_HEIGHT);
  painter.closePath();
}

function glow(
  painter: Painter,
  x: number,
  y: number,
  radiusX: number,
  radiusY: number,
  core: string,
  middle: string,
) {
  painter.save();
  painter.translate(x, y);
  painter.scale(radiusX, radiusY);
  const gradient = painter.createRadialGradient(0, 0, 0, 0, 0, 1);
  gradient.addColorStop(0, core);
  gradient.addColorStop(0.5, middle);
  gradient.addColorStop(1, "transparent");
  painter.fillStyle = gradient;
  painter.fillRect(-1, -1, 2, 2);
  painter.restore();
}

function paintSun(painter: Painter, x: number, y: number, elevation: number) {
  const low = 1 - elevation;
  glow(
    painter,
    x,
    HORIZON + 4,
    70 + 40 * low,
    26 + 16 * low,
    oklch(0.93, 0.11, 70, 0.3 + 0.5 * low),
    oklch(0.8, 0.16, 40, 0.08 + 0.26 * low),
  );
  const halo = BODY_RADIUS * 3.2;
  glow(painter, x, y, halo, halo, oklch(0.98, 0.05, 95, 0.55), oklch(0.95, 0.08, 80, 0.16));
  painter.fillStyle = oklch(0.82 + 0.16 * elevation, 0.16 - 0.1 * elevation, 50 + 45 * elevation);
  painter.beginPath();
  painter.arc(x, y, BODY_RADIUS, 0, Math.PI * 2);
  painter.fill();
}

function paintMoon(painter: Painter, x: number, y: number, phase: number) {
  const halo = BODY_RADIUS * 4;
  glow(painter, x, y, halo, halo, oklch(0.9, 0.04, 250, 0.3), oklch(0.8, 0.06, 255, 0.1));
  const radius = BODY_RADIUS * 0.9;
  // 1 at new moon, -1 at full: how far the shadow's edge bulges across the disc.
  const terminator = Math.cos(2 * Math.PI * phase);
  painter.save();
  painter.translate(x, y);
  if (phase >= 0.5) painter.scale(-1, 1);
  painter.fillStyle = oklch(0.95, 0.02, 95, 0.13);
  painter.beginPath();
  painter.arc(0, 0, radius, 0, Math.PI * 2);
  painter.fill();
  painter.fillStyle = oklch(0.96, 0.03, 95);
  painter.beginPath();
  painter.arc(0, 0, radius, -Math.PI / 2, Math.PI / 2);
  if (terminator > 0) {
    painter.ellipse(0, 0, radius * terminator, radius, 0, Math.PI / 2, -Math.PI / 2, true);
  } else {
    painter.ellipse(0, 0, -radius * terminator, radius, 0, Math.PI / 2, (3 * Math.PI) / 2);
  }
  painter.fill();
  painter.restore();
}

/** A screen of white dots, larger where the picture under them is brighter. */
function paintDotScreen(painter: Painter, width: number) {
  const pixelsPerPoint = painter.canvas.width / width;
  const picture = painter.getImageData(0, 0, painter.canvas.width, painter.canvas.height);
  for (let y = DOT_PITCH / 2; y < SKY_HEIGHT; y += DOT_PITCH) {
    for (let x = DOT_PITCH / 2; x < width; x += DOT_PITCH) {
      const pixel =
        (Math.floor(y * pixelsPerPoint) * picture.width + Math.floor(x * pixelsPerPoint)) * 4;
      const brightness =
        ((picture.data[pixel] ?? 0) * 0.3 +
          (picture.data[pixel + 1] ?? 0) * 0.59 +
          (picture.data[pixel + 2] ?? 0) * 0.11) /
        255;
      painter.fillStyle = `rgb(255 255 255 / ${0.08 + 0.18 * brightness})`;
      painter.beginPath();
      painter.arc(x, y, (0.25 + 0.75 * brightness) * DOT_PITCH * 0.42, 0, Math.PI * 2);
      painter.fill();
    }
  }
}

/**
 * Colours for controls that sit on the scene, as CSS custom properties: a disc in the far ridge's
 * tone, and the sun's or moon's light with a dark ink for the control under the pointer.
 */
export function skyControlColors(scene: SkyScene): Record<string, string> {
  const palette = paletteOf(scene);
  const [ridgeLightness, ridgeChroma, ridgeHue] = palette.farRidge;
  const [topLightness, topChroma, topHue] = palette.sky[0] ?? palette.farRidge;
  const sun = skyBodyAt(solarHourOf(scene)).sun;
  // At night the ridge is as dark as the sky behind the controls, so the disc steps away from it.
  const nightStep = palette.night * (scene.dark ? 0.08 : -0.04);
  return {
    "--sky-ridge": oklch(
      lifted(scene, ridgeLightness) - (scene.dark ? 0 : 0.06) + nightStep,
      ridgeChroma,
      ridgeHue,
      0.9,
    ),
    "--sky-body": sun ? oklch(0.93, 0.11, 80) : oklch(0.95, 0.03, 95),
    "--sky-body-glow": sun ? oklch(0.9, 0.14, 70, 0.7) : oklch(0.9, 0.05, 250, 0.6),
    "--sky-ink": oklch(
      Math.min(0.32, lifted(scene, topLightness) * 0.75),
      Math.min(0.12, topChroma),
      topHue,
    ),
  };
}

const midRidgeAt = (x: number) =>
  HORIZON + 5 - 6 * Math.sin(x / 23 + 2) - 2.5 * Math.sin(x / 9.3 + 0.7);

const foldAt = (x: number) => midRidgeAt(x) + 3.5 + 4.5 * Math.sin(x / 19 + 0.8);

// Gold and red against the orange slope; the tones between would be lost on it.
const AUTUMN_LEAVES: readonly SkyColor[] = [
  [0.86, 0.15, 90],
  [0.86, 0.15, 90],
  [0.52, 0.19, 27],
];

// What grows on the valley floor, per 256 points of width, from the crest `reach` points down.
const LAND_GROWTH: readonly {
  readonly season: keyof SkySeason;
  readonly shape: "bloom" | "broadleaf" | "larch" | "pine";
  readonly colors: readonly SkyColor[];
  readonly count: number;
  readonly reach: number;
}[] = [
  { season: "winter", shape: "pine", colors: [[0.36, 0.05, 170]], count: 16, reach: 7 },
  {
    season: "spring",
    shape: "bloom",
    colors: [
      [0.98, 0.02, 100],
      [0.98, 0.02, 100],
      [0.84, 0.11, 350],
      [0.84, 0.11, 350],
      [0.92, 0.14, 100],
    ],
    count: 29,
    reach: 4.6,
  },
  { season: "summer", shape: "pine", colors: [[0.36, 0.09, 152]], count: 14, reach: 7 },
  { season: "summer", shape: "bloom", colors: [[0.97, 0.03, 100]], count: 5, reach: 4.6 },
  { season: "autumn", shape: "larch", colors: AUTUMN_LEAVES, count: 9, reach: 7 },
  { season: "autumn", shape: "broadleaf", colors: AUTUMN_LEAVES, count: 5, reach: 7 },
  { season: "autumn", shape: "pine", colors: [[0.36, 0.07, 160]], count: 4, reach: 7 },
];

/** The valley floor in the season's colour, with what grows on it and the snow that lies on it. */
function paintLand(painter: Painter, scene: SkyScene, season: SkySeason, palette: SkyPalette) {
  const [lightness, chroma, hue] = skyGroundAt(season, palette);
  traceRidge(painter, scene.width, midRidgeAt);
  painter.fillStyle = oklch(lifted(scene, lightness) - (scene.dark ? 0 : 0.06), chroma, hue);
  painter.fill();

  // Snow settles on the crest first and reaches the valley as winter comes on.
  const snow = skySnowfieldAt(season, palette);
  const snowLineAt = (x: number) =>
    midRidgeAt(x) +
    snow.cover * 34 +
    (1.5 * Math.sin(x / 4.3 + 1) + Math.sin(x / 1.9)) * (1 - snow.cover);
  if (snow.cover > 0) {
    painter.save();
    painter.clip();
    painter.beginPath();
    painter.moveTo(0, 0);
    painter.lineTo(scene.width, 0);
    for (let x = scene.width; x >= 0; x--) painter.lineTo(x, snowLineAt(x));
    painter.closePath();
    const [snowLightness, snowChroma, snowHue] = snow.color;
    painter.fillStyle = oklch(lifted(scene, snowLightness), snowChroma, snowHue);
    painter.fill();
    painter.restore();
  }

  // A fold of shade across the valley and a line of light on its crest: one flat band reads bare.
  painter.save();
  traceRidge(painter, scene.width, midRidgeAt);
  painter.clip();
  traceRidge(painter, scene.width, foldAt);
  painter.fillStyle = `rgb(12 24 72 / ${scene.dark ? 0.2 : 0.1})`;
  painter.fill();
  painter.restore();
  painter.beginPath();
  for (let x = 0; x <= scene.width; x++) painter.lineTo(x, midRidgeAt(x) + 0.5);
  painter.strokeStyle = `rgb(255 255 255 / ${0.42 * (1 - 0.75 * palette.night)})`;
  painter.lineWidth = 0.9;
  painter.stroke();

  const dim = 1 - 0.45 * palette.night;
  const trunk = oklch(lifted(scene, 0.34 * dim), 0.04, 50);
  const plants = LAND_GROWTH.flatMap((kind, index) => {
    // The same places every day, so a season fills in and thins out instead of reshuffling.
    const random = seededRandom(4201 + index * 97);
    const count = Math.round((kind.count * scene.width) / 256);
    const fills = kind.colors.map(([plantLightness, plantChroma, plantHue]) =>
      oklch(lifted(scene, plantLightness * dim), plantChroma * dim, plantHue),
    );
    return Array.from({ length: count }, () => {
      const x = random() * scene.width;
      return {
        shape: kind.shape,
        x,
        // Thickest at the crest, where the tops stand against the mountains behind.
        y: midRidgeAt(x) + random() * random() * kind.reach,
        size: random(),
        fill: fills[Math.floor(random() * fills.length)] ?? "transparent",
        jitter: [random(), random(), random(), random()],
      };
    }).slice(0, Math.round(count * season[kind.season]));
  });
  // Far to near, so each tree stands in front of the ones up the slope.
  for (const { shape, x, y, size, fill, jitter } of plants.toSorted((a, b) => a.y - b.y)) {
    // Leaves, needles and flowers are gone where the snow lies; only the pines stand out of it.
    if (shape !== "pine" && snow.cover > 0 && y < snowLineAt(x)) continue;
    painter.beginPath();
    if (shape === "larch" || shape === "pine") {
      const half = 1.5 + 0.5 * size;
      const height = 5 + 2.2 * size;
      // Two tiers of boughs, the upper one narrower.
      painter.moveTo(x, y + 1 - height);
      painter.lineTo(x + half * 0.72, y + 1 - height * 0.42);
      painter.lineTo(x + half * 0.34, y + 1 - height * 0.42);
      painter.lineTo(x + half, y + 1);
      painter.lineTo(x - half, y + 1);
      painter.lineTo(x - half * 0.34, y + 1 - height * 0.42);
      painter.lineTo(x - half * 0.72, y + 1 - height * 0.42);
    } else if (shape === "broadleaf") {
      const lobe = 1.25 + 0.4 * size;
      painter.fillStyle = trunk;
      painter.fillRect(x - 0.4, y - 1.4, 0.8, 2.4);
      // Three lobes over a trunk: a crown, where one disc would be a mushroom.
      painter.arc(x - lobe * 0.8, y - 1.6 - lobe * 0.5, lobe, 0, Math.PI * 2);
      painter.arc(x + lobe * 0.8, y - 1.6 - lobe * 0.5, lobe, 0, Math.PI * 2);
      painter.arc(x, y - 1.6 - lobe * 1.7, lobe, 0, Math.PI * 2);
    } else {
      for (let dot = 0; dot <= Math.floor(size * 3); dot++) {
        const dotX = x + dot * 2 - 1 + (jitter[dot] ?? 0);
        const dotY = y + 1 + (dot % 2) * 1.2;
        painter.moveTo(dotX + 1.4, dotY);
        painter.arc(dotX, dotY, 1 + 0.4 * (jitter[dot + 1] ?? 0), 0, Math.PI * 2);
      }
    }
    painter.fillStyle = fill;
    painter.fill();
  }
}

/** Snow on the far ridge, from its crests down to a ragged line that winter lowers into the valley. */
function paintSnowCaps(painter: Painter, scene: SkyScene, snow: number, palette: SkyPalette) {
  const line = HORIZON - 19 + snow * 22;
  const [glowLightness, glowChroma, glowHue] = palette.sky[4] ?? palette.farRidge;
  painter.save();
  traceRidge(painter, scene.width, farRidgeAt);
  painter.clip();
  painter.beginPath();
  painter.moveTo(0, 0);
  painter.lineTo(scene.width, 0);
  for (let x = scene.width; x >= 0; x--) {
    // Tongues of snow reach down the slopes between bare ribs of rock.
    const tooth = Math.abs(((x / 6.5) % 2) - 1) * 4 + 1.6 * Math.sin(x / 3.7 + 2);
    painter.lineTo(x, line + tooth - 2);
  }
  painter.closePath();
  // Snow takes the light of the hour: white at noon, rose at dusk, blue under the moon.
  painter.fillStyle = oklch(
    lifted(scene, 0.97 - 0.5 * palette.night - 0.1 * Math.max(0, 0.85 - glowLightness)),
    0.012 + 0.03 * palette.night + 0.35 * glowChroma * (1 - palette.night),
    palette.night > 0.5 ? 250 : glowChroma > 0.08 ? glowHue : 245,
  );
  painter.fill();
  painter.restore();
}

/** The part that changes at most once a minute: sky, sun or moon, the far ridge and its haze. */
export function paintSkyBackdrop(painter: Painter, scene: SkyScene) {
  const palette = paletteOf(scene);
  const sky = painter.createLinearGradient(0, 0, 0, HORIZON + 10);
  const stops = [0, 0.4, 0.68, 0.88, 1];
  palette.sky.forEach((color, index) => sky.addColorStop(stops[index] ?? 1, paint(scene, color)));
  painter.fillStyle = sky;
  painter.fillRect(0, 0, scene.width, SKY_HEIGHT);

  const body = skyBodyAt(solarHourOf(scene));
  const elevation = Math.sin(Math.PI * body.progress);
  const bodyX = 16 + body.progress * (scene.width - 32);
  const { season } = scene;
  // The summer sun rides high and the winter sun stays low.
  const climb = body.sun
    ? SUN_CLIMB * (1 + 0.18 * season.summer - 0.36 * season.winter)
    : SUN_CLIMB;
  const bodyY = HORIZON + 5 - elevation * (climb + 5);
  if (body.sun) paintSun(painter, bodyX, bodyY, elevation);
  else paintMoon(painter, bodyX, bodyY, moonPhaseAt(scene.epochMs));

  const [ridgeLightness, ridgeChroma, ridgeHue] = palette.farRidge;
  traceRidge(painter, scene.width, farRidgeAt);
  painter.fillStyle = oklch(
    lifted(scene, ridgeLightness) - (scene.dark ? 0 : 0.06),
    ridgeChroma,
    ridgeHue,
  );
  painter.fill();

  paintSnowCaps(painter, scene, skySnowAt(season), palette);

  const [hazeLightness, hazeChroma, hazeHue] = palette.sky[3] ?? palette.farRidge;
  const haze = painter.createLinearGradient(0, HORIZON - 4, 0, HORIZON + 16);
  haze.addColorStop(0, "transparent");
  haze.addColorStop(
    1,
    oklch(lifted(scene, hazeLightness), hazeChroma * 0.6, hazeHue, scene.dark ? 0.16 : 0.3),
  );
  painter.fillStyle = haze;
  // To the foot of the picture: the near ridge dips below where the haze stops thickening.
  painter.fillRect(0, HORIZON - 4, scene.width, SKY_HEIGHT - HORIZON + 4);

  paintLand(painter, scene, season, palette);
  paintDotScreen(painter, scene.width);
}

function paintStars(painter: Painter, scene: SkyScene, night: number, seconds: number) {
  if (night <= 0.02) return;
  const random = seededRandom(3);
  const count = Math.round((34 * scene.width) / 255);
  for (let star = 0; star < count; star++) {
    const x = random() * scene.width;
    const y = random() * random() * (HORIZON - 14);
    const radius = 0.3 + random() * 0.5;
    const twinkle = 0.7 + 0.3 * Math.sin(seconds * (0.5 + random() * 1.6) + random() * 7);
    painter.fillStyle = `rgb(255 255 255 / ${0.85 * night * twinkle})`;
    painter.beginPath();
    painter.arc(x, y, radius, 0, Math.PI * 2);
    painter.fill();
  }
}

function paintClouds(painter: Painter, scene: SkyScene, palette: SkyPalette, seconds: number) {
  const random = seededRandom(scene.weather.seed);
  const [lightness, chroma, hue] = palette.sky[4] ?? palette.farRidge;
  painter.fillStyle = oklch(
    Math.min(0.97, lifted(scene, lightness) + 0.13),
    chroma * 0.7,
    hue,
    0.2 + 0.4 * (1 - palette.night * 0.6),
  );
  for (let cloud = 0; cloud < scene.weather.clouds; cloud++) {
    const length = 26 + random() * 46;
    const thickness = 2.6 + random() * 2.2;
    // Every third cloud rides high, above the header's controls; the rest stay under them.
    const y = cloud % 3 === 2 ? 3 + random() * 5 : 41 + random() * 11;
    const speed = (0.5 + random() * 0.9) * scene.weather.wind;
    const lap = scene.width + length + 50;
    const x = ((((random() * lap + speed * seconds) % lap) + lap) % lap) - length - 25;
    const rounding = thickness / 2;
    painter.beginPath();
    painter.roundRect(x, y, length, thickness, rounding);
    painter.roundRect(
      x + length * (0.12 + random() * 0.2),
      y - thickness * 0.62,
      length * (0.3 + random() * 0.25),
      thickness,
      rounding,
    );
    painter.roundRect(
      x + length * (0.45 + random() * 0.2),
      y + thickness * 0.62,
      length * (0.2 + random() * 0.25),
      thickness,
      rounding,
    );
    painter.fill();
  }
}

interface LiveEvent {
  readonly scene: SkyScene;
  readonly age: number;
  readonly lasts: number;
  readonly random: () => number;
}

/** Paints one event's current frame and returns the frame rate it needs. */
type EventPainter = (painter: Painter, event: LiveEvent) => number;

const fadeInOut = (event: LiveEvent, seconds: number) =>
  Math.min(1, event.age / seconds, (event.lasts - event.age) / seconds);

const paintAurora: EventPainter = (painter, event) => {
  const { scene, age, random } = event;
  const strength = fadeInOut(event, 8);
  const drift = random() * 50;
  painter.globalCompositeOperation = "lighter";
  const curtains = [
    { offset: 0, hue: 160, hueSpan: 35, top: 30, length: 30, alpha: 0.5 },
    { offset: 31, hue: 290, hueSpan: 35, top: 22, length: 26, alpha: 0.3 },
  ];
  for (const curtain of curtains) {
    for (let x = 0; x < scene.width; x++) {
      const band =
        0.5 +
        0.5 *
          Math.sin(x / 23 + curtain.offset + drift + age * 0.07) *
          Math.cos(x / 57 - age * 0.05 + curtain.offset);
      const rays = 0.6 + 0.4 * Math.sin(x / 3.1 + curtain.offset * 2 + age * 0.25);
      const light = Math.max(0, band * 1.5 - 0.4) * rays * strength;
      if (light < 0.02) continue;
      const top = curtain.top + 7 * Math.sin(x / 37 + curtain.offset + age * 0.04);
      const hue = curtain.hue + curtain.hueSpan * (x / scene.width);
      const ray = painter.createLinearGradient(0, top + curtain.length, 0, top - 8);
      ray.addColorStop(0, oklch(0.85, 0.18, hue, curtain.alpha * light));
      ray.addColorStop(0.3, oklch(0.72, 0.19, hue + 12, curtain.alpha * light * 0.6));
      ray.addColorStop(1, "transparent");
      painter.fillStyle = ray;
      painter.fillRect(x, top - 8, 1.05, curtain.length + 8);
    }
  }
  painter.globalCompositeOperation = "source-over";
  return 6;
};

const paintMeteor: EventPainter = (painter, { scene, age, lasts, random }) => {
  const progress = age / lasts;
  const startX = 24 + random() * (scene.width - 85);
  const startY = 3 + random() * 14;
  const direction = random() < 0.5 ? 1 : -1;
  const angle = 0.32 + random() * 0.3;
  const travelled = 72 * (1 - (1 - progress) ** 2);
  const brightness = Math.sin(Math.PI * progress);
  const headX = startX + direction * Math.cos(angle) * travelled;
  const headY = startY + Math.sin(angle) * travelled;
  const tailX = headX - direction * Math.cos(angle) * 24 * brightness;
  const tailY = headY - Math.sin(angle) * 24 * brightness;
  const trail = painter.createLinearGradient(headX, headY, tailX, tailY);
  trail.addColorStop(0, `rgb(255 255 255 / ${brightness})`);
  trail.addColorStop(1, "rgb(255 255 255 / 0)");
  painter.strokeStyle = trail;
  painter.lineWidth = 0.9;
  painter.lineCap = "round";
  painter.beginPath();
  painter.moveTo(headX, headY);
  painter.lineTo(tailX, tailY);
  painter.stroke();
  return 30;
};

const paintSatellite: EventPainter = (painter, { scene, age, lasts, random }) => {
  const progress = age / lasts;
  const startY = 4 + random() * 8;
  const slope = (random() - 0.3) * 0.12;
  const x = -5 + (scene.width + 10) * progress;
  painter.fillStyle = `rgb(255 255 255 / ${0.85 * Math.sin(Math.PI * progress) ** 0.6})`;
  painter.beginPath();
  painter.arc(x, startY + slope * x, 0.5, 0, Math.PI * 2);
  painter.fill();
  return 8;
};

const inkOf = (scene: SkyScene) => (scene.dark ? oklch(0.1, 0.03, 280) : oklch(0.26, 0.07, 285));

const paintFlock: EventPainter = (painter, { scene, age, lasts, random }) => {
  const birds = 3 + Math.floor(random() * 6);
  const direction = random() < 0.5 ? 1 : -1;
  const startY = 45 + random() * 9;
  const speed = (scene.width + 90) / lasts;
  const leaderX = direction > 0 ? -25 + speed * age : scene.width + 25 - speed * age;
  const leaderY = startY - age * 0.35 + Math.sin(age * 0.8) * 1.2;
  painter.strokeStyle = inkOf(scene);
  painter.lineWidth = 0.75;
  painter.lineCap = "round";
  painter.lineJoin = "round";
  for (let bird = 0; bird < birds; bird++) {
    // A loose V: birds alternate sides, each pair one rank further back.
    const side = bird % 2 ? 1 : -1;
    const rank = Math.ceil(bird / 2);
    const x = leaderX - direction * rank * (5 + random()) + random() * 1.5;
    const y = leaderY + side * rank * (2.4 + random() * 0.6) + Math.sin(age * 1.3 + bird) * 0.5;
    const gliding = Math.sin(age * 0.5 + bird * 0.7) > 0.6;
    const lift = gliding ? 0.3 : Math.sin((age / 0.5) * 2 * Math.PI + bird * 0.9);
    const wing = 2.5 + random() * 0.5;
    painter.beginPath();
    painter.moveTo(x - wing, y - lift * wing * 0.7);
    painter.quadraticCurveTo(x - wing * 0.45, y - lift * wing * 0.85 - 0.5, x, y);
    painter.quadraticCurveTo(
      x + wing * 0.45,
      y - lift * wing * 0.85 - 0.5,
      x + wing,
      y - lift * wing * 0.7,
    );
    painter.stroke();
  }
  return 24;
};

const WIND_SEGMENTS = 26;

const paintWind: EventPainter = (painter, { scene, age, random }) => {
  painter.lineCap = "round";
  for (let streak = 0; streak < 3; streak++) {
    const streakAge = age - streak * 0.4;
    const startX = -30 + random() * Math.max(40, scene.width - 145);
    const startY = 43 + random() * 14;
    const length = 130 + random() * 40;
    const wobble = random() * 6;
    const head = (streakAge / 1.6) * 1.35;
    if (streakAge < 0 || head > 1.35) continue;
    // The streak runs along a line that curls into one loop halfway.
    const pointAt = (along: number): [number, number] => {
      const loop = Math.min(1, Math.max(0, (along - 0.5) / 0.16));
      return [
        startX + length * along - 4 * Math.sin(2 * Math.PI * loop),
        startY + 2.5 * Math.sin(along * 5 + wobble) - 4 * (1 - Math.cos(2 * Math.PI * loop)),
      ];
    };
    for (let segment = 0; segment < WIND_SEGMENTS; segment++) {
      const from = head - 0.32 + (0.32 * segment) / WIND_SEGMENTS;
      const to = from + 0.32 / WIND_SEGMENTS;
      if (from < 0 || to > 1) continue;
      const fresh = segment / WIND_SEGMENTS;
      const [fromX, fromY] = pointAt(from);
      const [toX, toY] = pointAt(to);
      painter.strokeStyle = `rgb(255 255 255 / ${0.6 * fresh})`;
      painter.lineWidth = 0.35 + 0.5 * fresh;
      painter.beginPath();
      painter.moveTo(fromX, fromY);
      painter.lineTo(toX, toY);
      painter.stroke();
    }
  }
  return 30;
};

const BALLOON_HUES = [25, 255, 150, 60];

const paintBalloon: EventPainter = (painter, { scene, age, lasts, random }) => {
  const direction = random() < 0.5 ? 1 : -1;
  const travelled = ((scene.width + 24) * age) / lasts;
  const x = direction > 0 ? -12 + travelled : scene.width + 12 - travelled;
  const y = 49 + random() * 5 + Math.sin(age * 0.35) * 1.6 - age * 0.04;
  const hue = BALLOON_HUES[Math.floor(random() * BALLOON_HUES.length)] ?? 25;
  const ink = inkOf(scene);
  painter.save();
  painter.translate(x, y);
  painter.strokeStyle = ink;
  painter.lineWidth = 0.3;
  painter.beginPath();
  painter.moveTo(-1.6, 3);
  painter.lineTo(-0.7, 5.6);
  painter.moveTo(1.6, 3);
  painter.lineTo(0.7, 5.6);
  painter.stroke();
  painter.fillStyle = ink;
  painter.fillRect(-0.9, 5.4, 1.8, 1.3);
  painter.beginPath();
  painter.moveTo(0, -4.4);
  painter.bezierCurveTo(4.6, -4.4, 4.4, 1, 1.5, 3.4);
  painter.lineTo(-1.5, 3.4);
  painter.bezierCurveTo(-4.4, 1, -4.6, -4.4, 0, -4.4);
  painter.clip();
  painter.fillStyle = oklch(0.96, 0.03, 90);
  painter.fillRect(-5, -5, 10, 9);
  painter.fillStyle = oklch(0.66, 0.2, hue);
  for (const stripe of [-4.6, -1.1, 2.4]) painter.fillRect(stripe, -5, 1.75, 9);
  painter.restore();
  return 6;
};

const paintFireflies: EventPainter = (painter, event) => {
  const { scene, age, random } = event;
  const strength = fadeInOut(event, 4);
  for (let fly = 0; fly < 13; fly++) {
    const homeX = 10 + random() * (scene.width - 20);
    const homeY = HORIZON - 3 + random() * 13;
    const swayA = random() * 7;
    const swayB = random() * 7;
    const x = homeX + 6 * Math.sin(age * 0.3 + swayA) + 3 * Math.sin(age * 0.71 + swayB);
    const y = homeY + 2.5 * Math.sin(age * 0.43 + swayA + swayB);
    const pulse = Math.max(0, Math.sin(age * (0.9 + random() * 0.8) + random() * 7)) ** 2;
    const light = pulse * strength;
    if (light < 0.02) continue;
    glow(
      painter,
      x,
      y,
      4,
      4,
      oklch(0.93, 0.19, 115, 0.9 * light),
      oklch(0.86, 0.2, 120, 0.3 * light),
    );
    painter.fillStyle = oklch(0.98, 0.1, 110, light);
    painter.beginPath();
    painter.arc(x, y, 0.7, 0, Math.PI * 2);
    painter.fill();
  }
  return 10;
};

const paintDrift =
  (kind: "petals" | "leaves" | "snow"): EventPainter =>
  (painter, { scene, age, lasts, random }) => {
    const snow = kind === "snow";
    const flakes = snow ? 46 : 16;
    const life = snow ? 9 : 7;
    for (let flake = 0; flake < flakes; flake++) {
      const flakeAge = age - random() * (lasts - life);
      const startX = random() * (scene.width + 80) - 80;
      const flutter = random() * 7;
      const size = 0.7 + random() * 0.5;
      const leafHue = 30 + random() * 45;
      if (flakeAge < 0 || flakeAge > life) continue;
      const fallen = flakeAge / life;
      const x =
        startX + (snow ? 5 : 21) * flakeAge + (snow ? 2 : 4) * Math.sin(flakeAge * 2.2 + flutter);
      const y = -3 + (HORIZON + 12) * fallen + (snow ? 0 : 3 * Math.sin(flakeAge * 1.4 + flutter));
      const alpha = Math.min(1, fallen * 6, (1 - fallen) * 4);
      if (snow) {
        painter.fillStyle = `rgb(255 255 255 / ${0.85 * alpha})`;
        painter.beginPath();
        painter.arc(x, y, size * 0.7, 0, Math.PI * 2);
        painter.fill();
        continue;
      }
      painter.save();
      painter.translate(x, y);
      painter.rotate(flakeAge * 3 + flutter);
      painter.fillStyle =
        kind === "petals"
          ? oklch(0.9, 0.08, 355, 0.95 * alpha)
          : oklch(0.72, 0.17, leafHue, 0.95 * alpha);
      painter.beginPath();
      // The short radius swings as the petal tumbles, so it reads as turning over.
      const turned = size * 0.75 * Math.abs(Math.cos(flakeAge * 2.3 + flutter)) + 0.25;
      painter.ellipse(0, 0, size * 1.5, turned, 0, 0, Math.PI * 2);
      painter.fill();
      painter.restore();
    }
    return 20;
  };

const paintLanterns: EventPainter = (painter, { scene, age, random }) => {
  for (let lantern = 0; lantern < 8; lantern++) {
    const lanternAge = age - random() * 22;
    const startX = 14 + random() * (scene.width - 28);
    const sway = random() * 7;
    const rise = 0.75 + random() * 0.4;
    if (lanternAge < 0) continue;
    const x =
      startX + scene.weather.wind * 0.9 * lanternAge + 1.5 * Math.sin(lanternAge * 0.5 + sway);
    const y = HORIZON + 8 - rise * lanternAge;
    // Lanterns fade out before they reach the header's controls.
    const alpha = Math.min(1, lanternAge / 3, Math.max(0, (y - 38) / 8));
    if (alpha <= 0.01) continue;
    glow(
      painter,
      x,
      y,
      6,
      6,
      oklch(0.82, 0.17, 55, 0.55 * alpha),
      oklch(0.75, 0.18, 45, 0.16 * alpha),
    );
    painter.fillStyle = oklch(0.9, 0.14, 70, alpha);
    painter.beginPath();
    painter.roundRect(x - 1.1, y - 1.5, 2.2, 2.9, 0.7);
    painter.fill();
    painter.fillStyle = oklch(0.98, 0.07, 90, alpha);
    painter.fillRect(x - 0.5, y + 0.2, 1, 0.9);
  }
  return 8;
};

const FIREWORK_HUES = [85, 20, 200, 330, 140];
const FIREWORK_SPARKS = 24;

const paintFireworks: EventPainter = (painter, { scene, age, random }) => {
  painter.globalCompositeOperation = "lighter";
  for (let shell = 0; shell < 5; shell++) {
    const shellAge = age - (shell * 1.5 + random() * 0.5);
    const x = 16 + random() * (scene.width - 32);
    const burstY = 48 + random() * 6;
    const hue = FIREWORK_HUES[Math.floor(random() * FIREWORK_HUES.length)] ?? 85;
    const reach = 10 + random() * 4;
    const sparks = Array.from({ length: FIREWORK_SPARKS }, () => ({
      wobble: random() * 0.2,
      speed: 0.75 + random() * 0.3,
    }));
    if (shellAge < 0) continue;
    if (shellAge < 0.5) {
      const climbed = 1 - (1 - shellAge / 0.5) ** 2;
      const y = HORIZON + 4 - (HORIZON + 4 - burstY) * climbed;
      painter.strokeStyle = oklch(0.95, 0.06, 90, 0.7);
      painter.lineWidth = 0.5;
      painter.beginPath();
      painter.moveTo(x, y);
      painter.lineTo(x, y + 3);
      painter.stroke();
      continue;
    }
    const burstAge = shellAge - 0.5;
    if (burstAge > 1.7) continue;
    const radius = reach * (1 - Math.exp(-3.2 * burstAge));
    const alpha = Math.max(0, 1 - burstAge / 1.7) ** 1.4;
    const fall = 2.2 * burstAge * burstAge;
    sparks.forEach((spark, index) => {
      const angle = (index / FIREWORK_SPARKS) * 2 * Math.PI + spark.wobble;
      const sparkX = x + Math.cos(angle) * radius * spark.speed;
      const sparkY = burstY + Math.sin(angle) * radius * spark.speed + fall;
      const flicker = 0.6 + 0.4 * Math.sin(burstAge * 22 + index * 2.4);
      painter.fillStyle = oklch(0.93, 0.15, hue, alpha * flicker);
      painter.beginPath();
      painter.arc(sparkX, sparkY, 0.75, 0, Math.PI * 2);
      painter.fill();
      painter.strokeStyle = oklch(0.85, 0.18, hue, alpha * 0.5);
      painter.lineWidth = 0.45;
      painter.beginPath();
      painter.moveTo(sparkX, sparkY);
      painter.lineTo(
        sparkX - Math.cos(angle) * radius * 0.4,
        sparkY - Math.sin(angle) * radius * 0.4,
      );
      painter.stroke();
      if (index % 2 === 1) return;
      painter.fillStyle = oklch(0.96, 0.08, hue + 60, alpha * 0.9);
      painter.beginPath();
      painter.arc(
        x + Math.cos(angle + 0.13) * radius * 0.5,
        burstY + Math.sin(angle + 0.13) * radius * 0.5 + fall * 0.55,
        0.5,
        0,
        Math.PI * 2,
      );
      painter.fill();
    });
  }
  painter.globalCompositeOperation = "source-over";
  return 30;
};

/** Events that belong to the sky and are hidden by the far ridge. */
const SKY_PAINTERS: Partial<Record<SkyEvent["kind"], EventPainter>> = {
  aurora: paintAurora,
  meteor: paintMeteor,
  satellite: paintSatellite,
};

/** Events in front of the far ridge. */
const AIR_PAINTERS: Partial<Record<SkyEvent["kind"], EventPainter>> = {
  flock: paintFlock,
  wind: paintWind,
  balloon: paintBalloon,
  fireflies: paintFireflies,
  petals: paintDrift("petals"),
  leaves: paintDrift("leaves"),
  snow: paintDrift("snow"),
  lanterns: paintLanterns,
  fireworks: paintFireworks,
};

function paintEvents(
  painter: Painter,
  scene: SkyScene,
  painters: Partial<Record<SkyEvent["kind"], EventPainter>>,
  events: readonly SkyEvent[],
  epochSeconds: number,
): number {
  let framesPerSecond = 0;
  for (const event of events) {
    const paintEvent = painters[event.kind];
    const age = epochSeconds - event.startedAt;
    const lasts = skyEventSeconds(event.kind);
    if (!paintEvent || age < 0 || age > lasts) continue;
    const wanted = paintEvent(painter, { scene, age, lasts, random: seededRandom(event.seed) });
    framesPerSecond = Math.max(framesPerSecond, wanted);
  }
  return framesPerSecond;
}

/**
 * The part that moves, drawn over the backdrop: stars, clouds and the events in the air. It ends
 * by cutting the near ridge out of the picture, so the sidebar itself is the ground. Returns the
 * frame rate the motion needs, 0 when nothing in the picture moves.
 */
export function paintSkyLife(
  painter: Painter,
  scene: SkyScene,
  events: readonly SkyEvent[],
): number {
  const epochSeconds = scene.epochMs / 1000;
  const palette = paletteOf(scene);

  painter.save();
  painter.beginPath();
  painter.moveTo(0, 0);
  painter.lineTo(scene.width, 0);
  for (let x = scene.width; x >= 0; x--) painter.lineTo(x, farRidgeAt(x));
  painter.closePath();
  painter.clip();
  paintStars(painter, scene, palette.night, epochSeconds);
  const skyFrames = paintEvents(painter, scene, SKY_PAINTERS, events, epochSeconds);
  paintClouds(painter, scene, palette, epochSeconds);
  painter.restore();

  const airFrames = paintEvents(painter, scene, AIR_PAINTERS, events, epochSeconds);

  painter.globalCompositeOperation = "destination-out";
  traceRidge(painter, scene.width, nearRidgeAt);
  painter.fillStyle = "#000";
  painter.fill();
  painter.globalCompositeOperation = "source-over";

  // Stars twinkle and clouds drift; a cloudless day with nothing in the air holds still.
  const drifting = palette.night > 0.02 || scene.weather.clouds > 0;
  return Math.max(drifting ? IDLE_FRAMES_PER_SECOND : 0, skyFrames, airFrames);
}
