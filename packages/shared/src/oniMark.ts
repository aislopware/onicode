/**
 * The mochi oni from the app icon, shared by the desktop and mobile `OniWordmark`, redrawn for
 * small sizes: the icon's face and eyes inside an even rim. Each pose is a ring of outline points
 * (x, y pairs) and two eye curves (start, control, end). Both poses have the same point count and
 * matching landmarks, so one can morph into the other.
 */
export const ONI_MARK_VIEW_BOX = "108 128 566 500";
export const ONI_MARK_SIDES = ["left", "right"] as const;
/** The eyes' colour. The rim follows the text colour instead, and is drawn on light only. */
export const ONI_MARK_INK = "#0B0B0C";
/** The face gradient, bottom to top. */
export const ONI_MARK_FACE_STOPS = ["#FFFFFF", "#FBE6EC"] as const;
/** Stroke widths in mark units (the mark is 500 tall). `tiny` keeps marks under 14px legible. */
export const ONI_MARK_WEIGHTS = {
  regular: { rim: 48, eye: 41 },
  tiny: { rim: 60, eye: 50 },
} as const;

/** The resting pose: the app icon's three-quarter view, facing left. */
export const ONI_MARK_ANGLED = {
  outline: [
    353.9, 165.8, 361.8, 170, 365, 178.8, 367.1, 188, 369.9, 197, 373.7, 205.5, 378.8, 213.4, 385.5,
    219.8, 401.5, 220.6, 417.5, 221.8, 433.4, 224.1, 449.1, 227.2, 464.6, 231.2, 479.9, 235.9,
    494.9, 241.5, 510, 246.2, 518, 242.3, 525.3, 237, 531.7, 230.7, 537.4, 223.7, 542.5, 216.3,
    547.9, 209.2, 555.8, 205.6, 565.7, 212.3, 570.4, 223.7, 572.3, 236, 572.5, 248.5, 571.1, 260.8,
    568, 272.8, 565.2, 284.7, 588.7, 312, 607.9, 342.4, 621.4, 375.8, 628.5, 411, 628.2, 447, 620.3,
    482.1, 604.8, 514.6, 582.4, 542.6, 554, 564.7, 521.7, 580.5, 487, 590.1, 451.3, 594.7, 415.3,
    595, 379.4, 591.6, 344, 584.9, 309.4, 574.8, 276, 561.5, 244, 544.8, 214.6, 524.1, 188.6, 499.2,
    167.8, 469.8, 155.3, 436.2, 153.5, 400.3, 162.2, 365.5, 178, 333.1, 199.4, 304.2, 225.3, 279.1,
    254.6, 258.3, 286.7, 242, 320.3, 229.5, 322, 218.7, 323.6, 207.9, 326.6, 197.4, 330.8, 187.2,
    336.4, 177.8, 343.9, 169.8,
  ],
  eyes: {
    left: [241.6, 343.6, 283.9, 321.3, 326.5, 365.4],
    right: [433.4, 401, 477.8, 373.6, 522.1, 400.4],
  },
} as const;

/** Front-on. The animated desktop mark turns to it now and then. */
export const ONI_MARK_FRONT = {
  outline: [
    276.8, 165.8, 285.6, 171, 289.9, 180.8, 294, 190.7, 299.1, 200.2, 305.5, 208.8, 313.3, 216.2,
    323.6, 217.4, 340.2, 214.2, 357.1, 212.4, 374, 211.4, 391, 211.2, 408, 211.4, 424.9, 212.4,
    441.8, 214.2, 458.4, 217.4, 468.7, 216.2, 476.5, 208.8, 482.9, 200.2, 488, 190.7, 492.1, 180.8,
    496.4, 171, 505.2, 165.8, 516.8, 172.7, 523.9, 184.6, 528.3, 197.8, 530.8, 211.4, 531.3, 225.3,
    530.1, 239.1, 538.8, 248.8, 567, 270.4, 591.4, 296.3, 610.9, 326, 624.7, 358.7, 631.8, 393.5,
    631.6, 429, 623.8, 463.7, 608.7, 495.8, 586.8, 523.7, 559.6, 546.6, 528.9, 564.4, 495.9, 577.8,
    461.6, 587.1, 426.5, 592.8, 391, 595, 355.5, 592.8, 320.4, 587.1, 286.1, 577.8, 253.1, 564.4,
    222.4, 546.6, 195.2, 523.7, 173.3, 495.8, 158.2, 463.7, 150.4, 429, 150.2, 393.5, 157.3, 358.7,
    171.1, 326, 190.6, 296.3, 215, 270.4, 243.2, 248.8, 251.9, 239.1, 250.7, 225.3, 251.2, 211.4,
    253.7, 197.8, 258.1, 184.6, 265.2, 172.7,
  ],
  eyes: {
    left: [243.8, 378.7, 288.4, 348.5, 333.1, 378.7],
    right: [448.9, 378.7, 493.6, 348.5, 538.2, 378.7],
  },
} as const;

/** Where each ear's outline points start (at one base) and how many run over the tip to the other. */
export const ONI_MARK_EAR_POINTS = {
  left: { from: 59, count: 15 },
  right: { from: 15, count: 15 },
} as const;

/** A closed smooth path through outline points. Same point count in, same path commands out. */
export function oniMarkOutlinePath(points: ReadonlyArray<number>): string {
  const count = points.length / 2;
  const at = (i: number) => {
    const k = ((i % count) + count) % count;
    return [points[k * 2]!, points[k * 2 + 1]!] as const;
  };
  const tangent = (i: number) => {
    const [ax, ay] = at(i - 1);
    const [bx, by] = at(i + 1);
    const length = Math.hypot(bx - ax, by - ay) || 1;
    return [(bx - ax) / length, (by - ay) / length] as const;
  };
  const n = (value: number) => Math.round(value * 10) / 10;
  let path = `M${n(at(0)[0])} ${n(at(0)[1])}`;
  for (let i = 0; i < count; i += 1) {
    const [px, py] = at(i);
    const [qx, qy] = at(i + 1);
    const [tx, ty] = tangent(i);
    const [ux, uy] = tangent(i + 1);
    const reach = Math.hypot(qx - px, qy - py) / 3;
    path += `C${n(px + tx * reach)} ${n(py + ty * reach)} ${n(qx - ux * reach)} ${n(qy - uy * reach)} ${n(qx)} ${n(qy)}`;
  }
  return `${path}Z`;
}

export function oniMarkEyePath(eye: ReadonlyArray<number>): string {
  return `M${eye[0]} ${eye[1]}Q${eye[2]} ${eye[3]} ${eye[4]} ${eye[5]}`;
}
