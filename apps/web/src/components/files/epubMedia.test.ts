import { expect, it } from "vite-plus/test";

import { parseXml } from "./documentPreviewXml";
import { ttmlCaptions } from "./epubMedia";

const ttml = "http://www.w3.org/ns/ttml";
const dfxp = "http://www.w3.org/2006/04/ttaf1";
const smpte = 'p:timeBase="smpte" p:markerMode="continuous"';
const ntscRate = 'p:frameRate="30" p:frameRateMultiplier="1000 1001"';

it.each([
  ["frame clock times", ttml, "", "00:01:01:15", "00:01:03:00", "00:01:01.500 --> 00:01:03.000"],
  [
    "frame offsets at 25 fps",
    ttml,
    'p:frameRate="25"',
    "25f",
    "50f",
    "00:00:01.000 --> 00:00:02.000",
  ],
  [
    "frame offsets at a fractional DFXP rate",
    dfxp,
    'p:frameRate="24" p:frameRateMultiplier="1000 1001"',
    "24f",
    "48f",
    "00:00:01.001 --> 00:00:02.002",
  ],
  [
    "frame clock times with subframes",
    ttml,
    'p:frameRate="24" p:subFrameRate="2"',
    "00:00:01:12.1",
    "00:00:02:00.0",
    "00:00:01.521 --> 00:00:02.000",
  ],
  [
    "continuous non-drop SMPTE timecodes",
    ttml,
    `${smpte} p:dropMode="nonDrop" p:frameRate="25"`,
    "00:00:01:12",
    "00:00:02:00",
    "00:00:01.480 --> 00:00:02.000",
  ],
  [
    "continuous SMPTE timecodes at a fractional rate",
    ttml,
    `${smpte} ${ntscRate}`,
    "00:00:01:00",
    "00:00:02",
    "00:00:01.001 --> 00:00:02.002",
  ],
  [
    "continuous SMPTE NTSC drop-frame timecodes",
    ttml,
    `${smpte} ${ntscRate} p:dropMode="dropNTSC"`,
    "00:00:59:29",
    "00:01:00:02",
    "00:01:00.027 --> 00:01:00.060",
  ],
  [
    "continuous SMPTE PAL drop-frame timecodes",
    ttml,
    `${smpte} ${ntscRate} p:dropMode="dropPAL"`,
    "00:01:59:29",
    "00:02:00:04",
    "00:02:00.087 --> 00:02:00.120",
  ],
  // A clock without a frame label at a drop minute names the minute's first frame that exists.
  [
    "continuous SMPTE NTSC clocks without a frame label at a drop minute",
    ttml,
    `${smpte} ${ntscRate} p:dropMode="dropNTSC"`,
    "00:00:59:29",
    "00:01:00",
    "00:01:00.027 --> 00:01:00.060",
  ],
  [
    "continuous SMPTE PAL clocks without a frame label at a drop minute",
    ttml,
    `${smpte} ${ntscRate} p:dropMode="dropPAL"`,
    "00:01:59:29",
    "00:02:00",
    "00:02:00.087 --> 00:02:00.120",
  ],
  [
    "tick offsets at an explicit tick rate",
    ttml,
    'p:tickRate="10000000"',
    "10010000t",
    "30030000t",
    "00:00:01.001 --> 00:00:03.003",
  ],
  [
    "tick offsets defaulting to one tick per second",
    ttml,
    "",
    "2t",
    "4t",
    "00:00:02.000 --> 00:00:04.000",
  ],
  [
    "tick offsets derived from frame and subframe rates",
    ttml,
    'p:frameRate="25" p:subFrameRate="2"',
    "50t",
    "125t",
    "00:00:01.000 --> 00:00:02.500",
  ],
])("keeps captions using %s", (_label, namespace, parameters, begin, end, timing) => {
  const captions = parseXml(`
    <tt xmlns="${namespace}" xmlns:p="${namespace}#parameter" ${parameters}>
      <body><div><p begin="${begin}" end="${end}">Door closes</p></div></body>
    </tt>
  `);
  expect(ttmlCaptions(captions)).toBe(`WEBVTT\n\n${timing}\nDoor closes\n`);
});

it.each([
  [
    "a begin at a dropped NTSC frame label",
    `${smpte} ${ntscRate} p:dropMode="dropNTSC"`,
    'begin="00:01:00:00"',
  ],
  [
    "a begin whose drop minute has no frame left",
    `${smpte} p:frameRate="2" p:dropMode="dropNTSC"`,
    'begin="00:01:00"',
  ],
  ["a begin past the frame rate", 'p:frameRate="25"', 'begin="00:00:01:25"'],
  ["a fractional clock in SMPTE time", `${smpte} p:frameRate="25"`, 'begin="00:01:00.5"'],
  ["a seconds offset in SMPTE time", `${smpte} p:frameRate="25"`, 'begin="10s"'],
  ["an unknown begin", "", 'begin="soon"'],
  ["an unknown end", "", 'end="soon"'],
  ["an unknown dur", "", 'dur="soon"'],
])("leaves out a caption with %s", (_label, parameters, timing) => {
  const captions = parseXml(`
    <tt xmlns="${ttml}" xmlns:p="${ttml}#parameter" ${parameters}>
      <body><div end="00:01:30:00"><p ${timing}>Door closes</p></div></body>
    </tt>
  `);
  expect(ttmlCaptions(captions)).toBe("");
});
