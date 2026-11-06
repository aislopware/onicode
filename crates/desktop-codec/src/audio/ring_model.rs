//! The audio ring under loom: the decoder's thread and the device's render callback over every
//! interleaving of their atomics, a load free to read any of the stores loom keeps (the last
//! seven of each atomic), which is where a missing release or acquire shows: a cell no publish
//! was shown for still holds a NaN here (`UNWRITTEN`), which no check accepts. `cargo xtask deep
//! loom` runs it with `--cfg onicode_loom`, where `RING_SAMPLES` is four frames and
//! `FADE_FRAMES` one (`docs/decisions/tooling.md`, "The audio ring is model-checked with loom").
//!
//! Each published sample is [`VALUE`] plus its position, so what the device plays says where it
//! came from. Whatever the order, the device
//! - plays a run's samples once each, in order from the run's front, with nothing skipped;
//! - never plays a sample before the start's fade-in was written to it (a click);
//! - plays nothing of a muted run after anything of the next one;
//! - fills the rest of a render with the ramp from the last frame it played, then silence.

use loom::sync::Arc;

use super::{FADE_FRAMES, Ring, SAMPLES_PER_FRAME, Steer, fade_in_weight, playing};

/// A published sample is this plus its position.
const VALUE: f32 = 1000.0;

/// Frames the device asks for in one render.
const RENDER_FRAMES: usize = 2;

/// Samples in one render.
const RENDER_SAMPLES: usize = RENDER_FRAMES * SAMPLES_PER_FRAME;

/// The samples at positions `from..to`, as the decoder publishes them.
#[expect(clippy::cast_precision_loss, reason = "positions below eight")]
fn samples(from: usize, to: usize) -> Vec<f32> {
    (from..to).map(|p| VALUE + p as f32).collect()
}

/// What a run whose front is `front` plays when `published` samples are queued at its start:
/// the samples from the front on, the first [`FADE_FRAMES`] of them faded in.
fn run_of(front: usize, published: usize) -> Vec<[f32; SAMPLES_PER_FRAME]> {
    samples(front, published)
        .as_chunks::<SAMPLES_PER_FRAME>()
        .0
        .iter()
        .enumerate()
        .map(|(k, frame)| {
            let weight = if k < FADE_FRAMES {
                fade_in_weight(k)
            } else {
                1.0
            };
            frame.map(|s| s * weight)
        })
        .collect()
}

/// Read what the device played against `runs`, the runs the decoder started in order: each
/// frame is the next of the run playing, the first of a later run (the earlier one was muted or
/// ran dry), the ramp down from the frame played last, or silence. Anything else fails.
#[expect(
    clippy::float_cmp,
    reason = "the ring copies samples bit for bit: a match is exact"
)]
fn check_played(played: &[f32], runs: &[Vec<[f32; SAMPLES_PER_FRAME]>]) {
    let (mut run, mut next) = (0_usize, 0_usize);
    let mut last: Option<[f32; SAMPLES_PER_FRAME]> = None;
    let mut ramp = 0_usize;
    for (i, &frame) in played.as_chunks::<SAMPLES_PER_FRAME>().0.iter().enumerate() {
        let later = (run.saturating_add(1)..runs.len()).find(|&r| runs[r].first() == Some(&frame));
        if runs[run].get(next) == Some(&frame) {
            next = next.saturating_add(1);
            last = Some(frame);
            ramp = FADE_FRAMES;
        } else if let Some(r) = later {
            (run, next) = (r, 1);
            last = Some(frame);
            ramp = FADE_FRAMES;
        } else if frame == [0.0; SAMPLES_PER_FRAME] {
            ramp = 0;
        } else {
            #[expect(clippy::cast_precision_loss, reason = "a one-frame fade")]
            let gain = ramp as f32 / (FADE_FRAMES as f32 + 1.0);
            let tail = last.map(|t| t.map(|s| s * gain));
            assert!(
                ramp > 0 && tail == Some(frame),
                "frame {i} {frame:?} is not run {run}'s next ({:?}), a later run's first, a ramp \
                 from {last:?} or silence\nplayed {played:?}\nruns {runs:?}",
                runs[run].get(next),
            );
            ramp = ramp.saturating_sub(1);
        }
    }
}

/// Two renders of [`RENDER_FRAMES`] on the device's side, what they played in order.
fn render_twice(ring: &Ring) -> Vec<f32> {
    let mut played = Vec::new();
    for _ in 0..2 {
        let mut out = [f32::NAN; RENDER_SAMPLES];
        ring.pull(&mut out);
        played.extend_from_slice(&out);
    }
    played
}

/// A run started on two frames, with a third published while the device renders twice: it
/// plays from the front, in order, faded in, and runs dry into a ramp and silence.
#[test]
fn a_run_plays_what_was_published_once_and_in_order() {
    loom::model(|| {
        let ring = Arc::new(Ring::new());
        let device = {
            let ring = Arc::clone(&ring);
            loom::thread::spawn(move || render_twice(&ring))
        };
        let mut steer = Steer::default();
        steer.publish(&ring, &samples(0, 4));
        steer.start(&ring);
        steer.publish(&ring, &samples(4, 6));
        let played = device.join().unwrap();
        check_played(&played, &[run_of(0, 6)]);
    });
}

/// A mute while the device renders, then the next packets and a new run: nothing of the muted
/// run plays after the new one began, and the new one starts at its front, faded in, even when
/// the render copied its samples before the mute showed.
#[test]
fn a_mute_discards_the_run_and_the_next_one_starts_faded() {
    loom::model(|| {
        let ring = Arc::new(Ring::new());
        let device = {
            let ring = Arc::clone(&ring);
            loom::thread::spawn(move || render_twice(&ring))
        };
        let mut steer = Steer::default();
        steer.publish(&ring, &samples(0, 4));
        steer.start(&ring);
        steer.hold(&ring);
        steer.publish(&ring, &samples(4, 8));
        steer.start(&ring);
        let played = device.join().unwrap();
        check_played(&played, &[run_of(0, 4), run_of(4, 8)]);
    });
}

/// The device running dry and a mute both stop the run; whichever wins, the run is stopped once,
/// the decoder sees it, and the next run plays from the front the mute set.
#[test]
fn running_dry_and_a_mute_stop_the_run_once() {
    loom::model(|| {
        let ring = Arc::new(Ring::new());
        let device = {
            let ring = Arc::clone(&ring);
            loom::thread::spawn(move || {
                let mut out = [f32::NAN; RENDER_SAMPLES];
                ring.pull(&mut out);
                out.to_vec()
            })
        };
        let mut steer = Steer::default();
        steer.publish(&ring, &samples(0, 2));
        steer.start(&ring);
        steer.hold(&ring);
        let mut played = device.join().unwrap();
        assert!(!playing(ring.run.load(super::Ordering::Acquire)), "stopped");
        steer.observe(&ring);
        assert!(!steer.playing, "and the decoder knows it");
        steer.publish(&ring, &samples(2, 4));
        steer.start(&ring);
        let mut out = [f32::NAN; RENDER_SAMPLES];
        ring.pull(&mut out);
        played.extend_from_slice(&out);
        check_played(&played, &[run_of(0, 2), run_of(2, 4)]);
    });
}
