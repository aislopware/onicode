#!/usr/bin/env python3
"""Ten original auditions using Onicode's approved rounded pressure-pulse click.

Standalone standard-library synthesis. Only --install-attention changes an
application asset.
"""

import argparse
import json
import math
import struct
import wave
from pathlib import Path

RATE = 48000
MASTER = 0.55
# A click is a Ricker pulse, cut off this many widths from its center.
CLICK_SPAN = 5
PEAK_LIMIT = 0.13
# Harmonics are quiet, harmonic and rapidly damped; no noise or metallic modes.
COLORS = {
    "felt": [(1, 1, 1), (2, 0.075, 0.55), (3, 0.018, 0.35)],
    "warm": [(1, 1, 1), (2, 0.16, 0.60)],
    "clear": [(1, 1, 1), (2, 0.10, 0.55), (3, 0.035, 0.32)],
    "hollow": [(1, 1, 1), (3, 0.09, 0.40)],
}
# Clicks: onset, amplitude, width. Notes: onset, Hz, amplitude, decay, color.
CUES = [
    (
        "01-send",
        "Send accepted",
        "One compact click with a tiny muted note.",
        0.30,
        [(0.009, 0.20, 0.00062)],
        [(0.040, 293.66, 0.036, 0.045, "felt")],
    ),
    (
        "02-queued",
        "Added to queue",
        "Three diminishing taps, ending with a low warm hint.",
        0.48,
        [(0.009, 0.20, 0.00065), (0.099, 0.16, 0.00075), (0.229, 0.11, 0.00090)],
        [(0.255, 261.63, 0.030, 0.060, "warm")],
    ),
    (
        "03-upload-ready",
        "Upload ready",
        "Two clicks open a small ascending three-note flourish.",
        0.74,
        [(0.009, 0.15, 0.00080), (0.099, 0.20, 0.00058)],
        [
            (0.135, 261.63, 0.043, 0.070, "clear"),
            (0.220, 329.63, 0.038, 0.080, "clear"),
            (0.305, 392, 0.040, 0.105, "clear"),
        ],
    ),
    (
        "04-complete",
        "Completion — resolve",
        "A round pair with a descending, settled chime.",
        0.73,
        [(0.009, 0.17, 0.00055), (0.149, 0.21, 0.00088)],
        [(0.185, 392, 0.037, 0.080, "felt"), (0.305, 293.66, 0.050, 0.120, "felt")],
    ),
    (
        "05-question",
        "Question — open interval",
        "A patient pair followed by two gently rising notes.",
        0.79,
        [(0.009, 0.16, 0.00090), (0.219, 0.20, 0.00058)],
        [(0.250, 329.63, 0.038, 0.085, "hollow"), (0.365, 440, 0.035, 0.120, "hollow")],
    ),
    (
        "06-attention",
        "Needs attention",
        "Two broader taps and a restrained downward step.",
        0.65,
        [(0.009, 0.19, 0.00095), (0.189, 0.17, 0.00082)],
        [(0.225, 349.23, 0.043, 0.085, "warm"), (0.335, 311.13, 0.039, 0.100, "warm")],
    ),
    (
        "07-mic-on",
        "Microphone on",
        "A light opening pair with a quick upward fifth.",
        0.55,
        [(0.009, 0.14, 0.00082), (0.089, 0.18, 0.00054)],
        [(0.120, 261.63, 0.035, 0.055, "clear"), (0.215, 392, 0.038, 0.075, "clear")],
    ),
    (
        "08-mic-off",
        "Microphone off",
        "The closing counterpart: broader click and downward fifth.",
        0.55,
        [(0.009, 0.18, 0.00054), (0.109, 0.14, 0.00082)],
        [(0.135, 392, 0.032, 0.050, "felt"), (0.225, 261.63, 0.038, 0.075, "felt")],
    ),
    (
        "09-reconnected",
        "Connection restored",
        "Three growing taps land on a soft two-note harmony.",
        0.76,
        [(0.009, 0.11, 0.00090), (0.139, 0.15, 0.00074), (0.249, 0.20, 0.00060)],
        [(0.280, 293.66, 0.041, 0.120, "warm"), (0.290, 369.99, 0.029, 0.110, "felt")],
    ),
    (
        "10-undo",
        "Undo / restore",
        "Two reversed-weight clicks with a brief falling chime.",
        0.49,
        [(0.009, 0.20, 0.00055), (0.099, 0.12, 0.00088)],
        [(0.125, 329.63, 0.031, 0.055, "hollow"), (0.200, 293.66, 0.037, 0.065, "hollow")],
    ),
]


def render(spec: tuple) -> tuple[list[list[float]], float]:
    """Synthesize one cue; its stereo frames and peak."""
    _name, _label, _description, duration, clicks, notes = spec
    count = round(duration * RATE)
    dry = []
    for i in range(count):
        t = i / RATE
        click = 0.0
        for center, amplitude, width in clicks:
            x = (t - center) / width
            if abs(x) < CLICK_SPAN:
                click += amplitude * (1 - 2 * x * x) * math.exp(-x * x)
        tone = 0.0
        for start, frequency, amplitude, decay, color in notes:
            u = t - start
            if u < 0:
                continue
            for ratio, level, damping in COLORS[color]:
                envelope = (1 - math.exp(-u / 0.010)) * math.exp(-u / (decay * damping))
                tone += amplitude * level * envelope * math.sin(2 * math.pi * frequency * ratio * u)
        dry.append((click, tone))
    frames = []
    for i, (click, tone) in enumerate(dry):
        channels = []
        for delay in [0.026, 0.035]:
            k = i - round(delay * RATE)
            reflection = 0.045 * dry[k][1] if k >= 0 else 0
            fade = min(1, (count - i) / (0.060 * RATE))
            channels.append((click + tone + reflection) * MASTER * fade)
        frames.append(channels)
    peak = max(abs(v) for frame in frames for v in frame)
    if peak >= PEAK_LIMIT:
        msg = f"peak {peak:.3f} is over {PEAK_LIMIT}: keep the family quiet without normalization"
        raise ValueError(msg)
    return frames, peak


def write(path: Path, frames: list[list[float]]) -> None:
    """Write stereo 16-bit PCM at RATE."""
    with wave.open(str(path), "wb") as f:
        f.setnchannels(2)
        f.setsampwidth(2)
        f.setframerate(RATE)
        f.writeframes(b"".join(struct.pack("<hh", *(round(v * 32767) for v in frame)) for frame in frames))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output-dir", type=Path, default=Path(__file__).resolve().parents[1] / "docs/sound-design/auditions"
    )
    parser.add_argument(
        "--install-attention",
        action="store_true",
        help="Also install the selected attention cue as the application asset.",
    )
    args = parser.parse_args()
    folder = args.output_dir
    folder.mkdir(parents=True, exist_ok=True)
    manifest = []
    for cue in CUES:
        frames, peak = render(cue)
        write(folder / (cue[0] + ".wav"), frames)
        if args.install_attention and cue[0] == "06-attention":
            asset = Path(__file__).resolve().parents[1] / "crates/ui/assets/sounds/attention.wav"
            write(asset, frames)
        manifest.append(
            {
                "file": cue[0] + ".wav",
                "name": cue[1],
                "description": cue[2],
                "duration": cue[3],
                "peak_dbfs": round(20 * math.log10(peak), 1),
            }
        )
    (folder / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    rows = [
        "# Rounded sound family — ten additional auditions\n",
        (
            "Original synthesis using the approved rounded clicks, with wider rhythmic and tonal variety. Quiet "
            "levels, no noisy tails or metallic impacts.\n"
        ),
        (
            "The numbered files remain audition references. `06-attention.wav` is also the source for the "
            "selected application attention cue.\n"
        ),
        "| # | Intended action | Character | Duration | Peak |",
        "|---|---|---|---:|---:|",
    ]
    for n, item in enumerate(manifest, 1):
        rows.append(
            f"| {n:02} | [{item['name']}]({item['file']}) | {item['description']} "
            f"| {item['duration']:.2f}s | {item['peak_dbfs']:.1f} dBFS |"
        )
    rows += [
        (
            "\nRegenerate with `python3 scripts/generate-sound-auditions.py`; pass `--install-attention` to "
            "refresh the selected runtime copy. Stereo 48 kHz, 16-bit PCM. No external samples. Perceived "
            "loudness depends on playback; no upward normalization is applied."
        ),
        (
            "\nThe action names are audition contexts, not recommendations to enable all ten. Frequent actions "
            "such as Send and Undo stay silent; the product sound scope is intentionally limited to completion, "
            "input required, errors or durable disconnections, and Appshot capture."
        ),
    ]
    (folder / "README.md").write_text("\n".join(rows) + "\n")
    print(json.dumps(manifest, indent=2))
