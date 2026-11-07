//! The cursor as the worker reads it, against a cursor of known colours that a child app
//! (`tests/support/cursor_app.rs`) shows; gated by `ONICODE_DESKTOP_LIVE=1`, since it changes the
//! cursor on screen for as long as it runs (well under a second) and needs a window session.

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use onicode_desktop_proto::screen::CursorShape;

    /// The premultiplied BGRA pixel at `(fx, fy)`, fractions of the picture; clear when the
    /// picture has none there.
    fn pixel(shape: &CursorShape, fx: f32, fy: f32) -> [u8; 4] {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a fraction of a cursor's side"
        )]
        #[expect(clippy::cast_sign_loss, reason = "a fraction of a positive side")]
        let (x, y) = (
            (f32::from(shape.w) * fx) as usize,
            (f32::from(shape.h) * fy) as usize,
        );
        y.checked_mul(usize::from(shape.w))
            .and_then(|row| row.checked_add(x))
            .and_then(|at| shape.bgra.as_chunks::<4>().0.get(at).copied())
            .unwrap_or_default()
    }

    /// Whether `px` (BGRA) is mostly the channel at `index` and little of the other colours:
    /// loose enough for the window server's colour matching, tight enough that a swap of red
    /// and blue fails.
    fn mostly(px: [u8; 4], index: usize) -> bool {
        let [b, g, r, a] = px;
        a > 200
            && [b, g, r]
                .iter()
                .enumerate()
                .all(|(c, v)| if c == index { *v > 180 } else { *v < 90 })
    }

    /// Block this test thread while the child's cursor comes up.
    fn pause(d: Duration) {
        std::thread::sleep(d);
    }

    fn is_the_test_cursor(shape: &CursorShape) -> bool {
        mostly(pixel(shape, 0.25, 0.25), 2)
            && mostly(pixel(shape, 0.75, 0.25), 1)
            && mostly(pixel(shape, 0.25, 0.75), 0)
    }

    /// Red, green and blue come back in BGRA order where the child drew them, white at half
    /// cover comes back premultiplied, and the hotspot is at (3, 5) of 16 points in pixels at
    /// the picture's scale.
    #[test]
    fn a_coloured_cursor_reads_back_in_bgra_with_its_hotspot_at_its_scale() {
        if std::env::var_os("ONICODE_DESKTOP_LIVE").is_none() {
            onicode_desktop_testkit::live::skip("set ONICODE_DESKTOP_LIVE=1");
            return;
        }
        let mut app = Command::new(env!("CARGO_BIN_EXE_onicode-cursor-app"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the cursor app starts");
        let mut lines = BufReader::new(app.stdout.take().expect("its stdout")).lines();
        let ready = lines.next().and_then(Result::ok).unwrap_or_default();
        assert!(ready.starts_with("ready"), "the app said {ready:?}");

        let _warm = onicode_desktop_capture::warm_cursor();
        let started = Instant::now();
        let mut last = None;
        let shape = loop {
            if let Some(shape) = onicode_desktop_capture::read_cursor() {
                if is_the_test_cursor(&shape) {
                    break Some(shape);
                }
                last = Some(shape);
            }
            if started.elapsed() > Duration::from_secs(5) {
                break None;
            }
            pause(Duration::from_millis(10));
        };
        drop(app.stdin.take());
        let _reaped = app.wait();
        let shape = shape.unwrap_or_else(|| {
            let seen = last.map(|s| (s.w, s.h, s.scale, pixel(&s, 0.25, 0.25)));
            panic!("the test cursor never came up; last read (w, h, scale, top left): {seen:?}")
        });

        let scale = u16::from(shape.scale);
        let side = scale.saturating_mul(16);
        eprintln!(
            "{}×{} at {}×, hotspot ({}, {}); quadrants {:?} {:?} {:?} {:?}",
            shape.w,
            shape.h,
            shape.scale,
            shape.hot_x,
            shape.hot_y,
            pixel(&shape, 0.25, 0.25),
            pixel(&shape, 0.75, 0.25),
            pixel(&shape, 0.25, 0.75),
            pixel(&shape, 0.75, 0.75)
        );
        assert_eq!(
            (shape.w, shape.h),
            (side, side),
            "16 points at {}×",
            shape.scale
        );
        assert_eq!(
            (shape.hot_x, shape.hot_y),
            (scale.saturating_mul(3), scale.saturating_mul(5)),
            "the hotspot in pixels"
        );
        let [b, g, r, a] = pixel(&shape, 0.75, 0.75);
        assert!((100..=156).contains(&a), "half cover: alpha {a}");
        assert!(
            [b, g, r].iter().all(|c| c.abs_diff(a) <= 16),
            "white premultiplied by its cover: {:?}",
            [b, g, r, a]
        );
    }
}
