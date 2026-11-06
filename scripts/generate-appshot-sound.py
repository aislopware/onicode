#!/usr/bin/env python3
"""Generate the original rounded Appshot confirmation cue.

Three separated shutter clicks followed by a single smooth C4-F4 resonance, adapted
from the rounded sound-family auditions. Standard-library-only, deterministic
stereo PCM; no external samples, noise bed or upward normalization.
"""

import math
import struct
import wave
from pathlib import Path

RATE = 48000
MASTER = 0.55
# A click is a Ricker pulse, cut off this many widths from its center.
CLICK_SPAN = 5
PEAK_LIMIT = 0.13
COLORS = {
    "felt": [(1, 1, 1), (2, 0.075, 0.55), (3, 0.018, 0.35)],
    "clear": [(1, 1, 1), (2, 0.10, 0.55), (3, 0.035, 0.32)],
}
# Clicks: onset, amplitude, width. Notes: onset, Hz, amplitude, decay, color.
APPSHOT = (
    "appshot",
    "Appshot captured",
    "Round shutter triple and blended confirmation",
    0.67,
    [(0.009, 0.19, 0.00075), (0.079, 0.21, 0.00058), (0.149, 0.18, 0.00068)],
    [
        (0.180, 261.63, 0.030, 0.120, "felt"),
        (0.180, 349.23, 0.038, 0.120, "felt"),
        (0.180, 523.25, 0.009, 0.085, "clear"),
    ],
)


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
                envelope = (1 - math.exp(-u / 0.018)) * math.exp(-u / (decay * damping))
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
    frames, peak = render(APPSHOT)
    output = Path(__file__).resolve().parents[1] / "crates/ui/assets/sounds/appshot.wav"
    write(output, frames)
    print(f"{output}: {len(frames) / RATE:.2f}s, stereo, peak {20 * math.log10(peak):.1f} dBFS")
