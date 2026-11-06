//! The one test that makes a real virtual display. Creating it rearranges the Mac's screens
//! under whoever is using them, so it is ignored and also wants `ONICODE_VDISPLAY_LIVE=1`:
//! `ONICODE_VDISPLAY_LIVE=1 cargo nextest run -E 'test(a_virtual_display_takes_its_mode)'
//! --run-ignored only`.

#[cfg(test)]
mod tests {
    use std::process::Command;

    #[test]
    #[ignore = "creates a display, rearranging the screens of whoever uses this Mac"]
    fn a_virtual_display_takes_its_mode_rotates_and_goes_away() {
        if std::env::var_os("ONICODE_VDISPLAY_LIVE").is_none_or(|v| v != "1") {
            onicode_desktop_testkit::live::skip("set ONICODE_VDISPLAY_LIVE=1 to create a display");
            return;
        }
        let out = Command::new(env!("CARGO_BIN_EXE_onicode-vdisplay-probe"))
            .output()
            .unwrap();
        let report = String::from_utf8_lossy(&out.stderr);
        eprintln!("{report}");
        assert!(out.status.success(), "{report}");
    }
}
