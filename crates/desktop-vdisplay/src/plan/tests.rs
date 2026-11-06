use super::*;

fn request(pixels: (u32, u32), scale: f64, refresh_hz: u32) -> Request {
    Request {
        pixels,
        scale,
        refresh_hz,
        client: ClientKey::new(b"test client"),
    }
}

#[test]
fn a_5k_retina_client_gets_a_2x_mode_at_its_native_size() {
    let plan = plan(&request((5120, 2880), 2.0, 60));
    assert_eq!(
        plan.mode,
        Mode {
            points: (2560, 1440),
            pixels: (5120, 2880),
            hidpi: true,
            refresh_hz: 60
        }
    );
    assert_eq!(plan.descriptor.max_pixels, (6400, 6400));
    // Apple's 27" 5K panel is 597 × 336 mm.
    let (w, h) = plan.descriptor.size_mm;
    assert!(
        (w - 596.6).abs() < 0.5 && (h - 335.6).abs() < 0.5,
        "{w} × {h}"
    );
}

#[test]
fn an_ipad_rotates_within_the_display_it_was_created_with() {
    let portrait = plan(&request((2064, 2752), 2.0, 120));
    let landscape = plan(&request((2752, 2064), 2.0, 120));
    assert_eq!(portrait.mode.points, (1032, 1376));
    assert_eq!(landscape.mode.points, (1376, 1032));
    assert_eq!(portrait.mode.refresh_hz, 120);
    assert_eq!(
        portrait.descriptor.max_pixels,
        landscape.descriptor.max_pixels
    );
    assert!(landscape.mode.fits(portrait.descriptor.max_pixels));
    assert_eq!(portrait.descriptor.serial, landscape.descriptor.serial);
}

#[test]
fn a_standard_1080p_client_gets_a_1x_mode_at_109_ppi() {
    let plan = plan(&request((1920, 1080), 1.0, 60));
    assert_eq!(
        plan.mode,
        Mode {
            points: (1920, 1080),
            pixels: (1920, 1080),
            hidpi: false,
            refresh_hz: 60
        }
    );
    assert_eq!(plan.descriptor.max_pixels, (2432, 2432));
    let (w, _) = plan.descriptor.size_mm;
    assert!((w - 1920.0 * 25.4 / 109.0).abs() < 1e-9);
}

#[test]
fn odd_sides_round_down_to_what_the_encoder_takes() {
    let standard = plan(&request((1919, 1081), 1.0, 60));
    assert_eq!(standard.mode.pixels, (1918, 1080));
    let retina = plan(&request((2881, 1799), 2.0, 60));
    assert_eq!(retina.mode.points, (1440, 899));
    assert_eq!(retina.mode.pixels, (2880, 1798));
}

#[test]
fn scales_below_2_or_not_finite_are_1x_and_above_2_are_2x() {
    for scale in [0.0, -2.0, 1.0, 1.5, 1.999, f64::NAN, f64::INFINITY] {
        assert!(
            !plan(&request((1920, 1080), scale, 60)).mode.hidpi,
            "{scale}"
        );
    }
    let phone = plan(&request((1290, 2796), 3.0, 120));
    assert!(phone.mode.hidpi);
    assert_eq!(phone.mode.points, (645, 1398));
}

#[test]
fn sizes_clamp_to_8k_keeping_the_aspect_and_to_a_floor() {
    let huge = plan(&request((16000, 9000), 1.0, 60));
    assert_eq!(huge.mode.pixels, (7680, 4320));
    assert_eq!(huge.descriptor.max_pixels, (7680, 7680));
    let huge_retina = plan(&request((16000, 9000), 2.0, 60));
    assert_eq!(huge_retina.mode.points, (3840, 2160));
    assert_eq!(huge_retina.mode.pixels, (7680, 4320));
    let tiny = plan(&request((200, 100), 2.0, 60));
    assert_eq!(tiny.mode.points, (MIN_SIDE_POINTS, MIN_SIDE_POINTS));
    let empty = plan(&request((0, 0), 1.0, 60));
    assert_eq!(empty.mode.pixels, (MIN_SIDE_POINTS, MIN_SIDE_POINTS));
}

#[test]
fn refresh_defaults_to_60_and_clamps_to_30_through_120() {
    assert_eq!(plan(&request((1920, 1080), 1.0, 0)).mode.refresh_hz, 60);
    assert_eq!(plan(&request((1920, 1080), 1.0, 240)).mode.refresh_hz, 120);
    assert_eq!(plan(&request((1920, 1080), 1.0, 10)).mode.refresh_hz, 30);
    assert_eq!(plan(&request((1920, 1080), 1.0, 90)).mode.refresh_hz, 90);
}

#[test]
fn every_plan_is_even_consistent_and_within_its_maximum() {
    let sides = [
        0, 1, 479, 480, 961, 1080, 1366, 2064, 2881, 5120, 7680, 7681, 20000,
    ];
    for &w in &sides {
        for &h in &sides {
            for scale in [1.0, 2.0, 3.0] {
                let Plan { descriptor, mode } = plan(&request((w, h), scale, 60));
                let backing = if mode.hidpi { 2 } else { 1 };
                assert_eq!(
                    mode.pixels,
                    (mode.points.0 * backing, mode.points.1 * backing)
                );
                assert!(mode.pixels.0 % 2 == 0 && mode.pixels.1 % 2 == 0, "{mode:?}");
                assert!(mode.points.0 >= MIN_SIDE_POINTS && mode.points.1 >= MIN_SIDE_POINTS);
                assert!(mode.pixels.0 <= MAX_SIDE_PIXELS && mode.pixels.1 <= MAX_SIDE_PIXELS);
                assert!(
                    mode.fits(descriptor.max_pixels),
                    "{w}×{h}@{scale}: {descriptor:?}"
                );
                assert!(descriptor.max_pixels.0 <= MAX_SIDE_PIXELS);
            }
        }
    }
}

#[test]
fn a_client_key_is_stable_across_builds_and_distinct_per_client() {
    // FNV-1a's offset basis: pinned so a changed hash, which would orphan every arrangement
    // macOS stored for a client, fails here.
    let empty = ClientKey::new(b"");
    assert_eq!((empty.product_id(), empty.serial()), (0xcbf2, 0x8422_2325));
    let a = ClientKey::new(b"ipad-3f2c");
    assert_eq!(a, ClientKey::new(b"ipad-3f2c"));
    let b = ClientKey::new(b"ipad-3f2d");
    assert_ne!((a.product_id(), a.serial()), (b.product_id(), b.serial()));
    for key in [a, b, empty] {
        assert!(key.product_id() <= 0xffff && key.serial() != 0);
    }
}

#[test]
fn the_vendor_is_oni_in_edid_letters() {
    let letter =
        |shift: u32| char::from(b'A' - 1 + u8::try_from((VENDOR_ID >> shift) & 31).unwrap());
    assert_eq!([letter(10), letter(5), letter(0)], ['O', 'N', 'I']);
}

#[test]
fn a_mode_matches_only_its_own_listing() {
    let mode = Mode {
        points: (2560, 1440),
        pixels: (5120, 2880),
        hidpi: true,
        refresh_hz: 60,
    };
    assert!(mode.matches((2560, 1440), (5120, 2880), 60.0));
    assert!(mode.matches((2560, 1440), (5120, 2880), 0.0));
    assert!(mode.matches((2560, 1440), (5120, 2880), 59.94));
    assert!(
        !mode.matches((2560, 1440), (2560, 1440), 60.0),
        "the duplicate low-resolution mode"
    );
    assert!(!mode.matches((2560, 1440), (5120, 2880), 120.0));
    assert!(!mode.matches((1440, 2560), (2880, 5120), 60.0));
}
