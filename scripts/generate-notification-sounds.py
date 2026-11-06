#!/usr/bin/env python3
"""Original rounded notification family; standard-library-only stereo PCM.

The selected Done and Request cues replace existing assets without changing
notification triggers. Optional minimal alternatives are audition files only.
No external samples, noisy tails, resonant impacts or upward normalization.
"""

import argparse
import math
import struct
import wave
from pathlib import Path

RATE = 48000
MASTER_GAIN = 0.55
# A click is a Ricker pulse, cut off this many widths from its center.
CLICK_SPAN = 5
PEAK_LIMIT = 0.14
# Each cue: its duration, its clicks as center, amplitude and width, and its tones as start, frequency,
# amplitude and decay.
CUES = {
    # A brighter tap settles into a broader, softer final click.
    "done": (0.52, [(0.009, 0.18, 0.00052), (0.119, 0.21, 0.00085)], [(0.145, 349.23, 0.055, 0.10)]),
    # The second tap is brighter and slightly stronger: a gentle invitation.
    "request": (0.60, [(0.009, 0.16, 0.00085), (0.179, 0.21, 0.00052)], [(0.205, 392.00, 0.05, 0.12)]),
    # Audition alternatives use only tactile clicks, without a tonal tail.
    "done-minimal": (0.18, [(0.009, 0.21, 0.00085)], []),
    "request-minimal": (0.38, [(0.009, 0.17, 0.00075), (0.179, 0.21, 0.00052), (0.259, 0.12, 0.00060)], []),
}


def synthesize(cue: tuple) -> tuple[list[list[float]], float]:
    """Synthesize one cue; its stereo frames and peak."""
    duration, clicks, tones = cue
    count = round(RATE * duration)
    dry = []
    for index in range(count):
        t = index / RATE
        click = 0.0
        for center, amplitude, width in clicks:
            position = (t - center) / width
            if abs(position) < CLICK_SPAN:
                click += amplitude * (1 - 2 * position * position) * math.exp(-position * position)
        tone = 0.0
        for start, frequency, amplitude, decay in tones:
            u = t - start
            if u >= 0:
                envelope = (1 - math.exp(-u / 0.009)) * math.exp(-u / decay)
                tone += (
                    amplitude
                    * envelope
                    * (math.sin(2 * math.pi * frequency * u) + 0.12 * math.sin(2 * math.pi * frequency * 2 * u))
                )
        dry.append((click, tone))
    frames = []
    for index, (click, tone) in enumerate(dry):
        channels = []
        for delay in [0.024, 0.032]:
            offset = index - round(delay * RATE)
            reflection = 0.06 * dry[offset][1] if offset >= 0 else 0.0
            fade = min((count - index) / (0.075 * RATE), 1.0)
            channels.append((click + tone + reflection) * fade * MASTER_GAIN)
        frames.append(channels)
    peak = max(abs(value) for frame in frames for value in frame)
    if peak >= PEAK_LIMIT:
        msg = "Keep the entire family quiet without normalization."
        raise ValueError(msg)
    return frames, peak


def write_cue(name: str, destination: Path) -> None:
    """Render a cue to a WAV file."""
    frames, peak = synthesize(CUES[name])
    destination.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(destination), "wb") as wav:
        wav.setnchannels(2)
        wav.setsampwidth(2)
        wav.setframerate(RATE)
        wav.writeframes(b"".join(struct.pack("<hh", *(round(v * 32767) for v in frame)) for frame in frames))
    print(f"{destination}: {len(frames) / RATE:.2f}s, peak {20 * math.log10(peak):.1f} dBFS")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--variants-dir", type=Path, help="Also export all cues for audition.")
    args = parser.parse_args()
    assets = Path(__file__).resolve().parents[1] / "crates/ui/assets/sounds"
    for name in ["done", "request"]:
        write_cue(name, assets / f"{name}.wav")
    if args.variants_dir:
        for name in CUES:
            write_cue(name, args.variants_dir / f"{name}.wav")
