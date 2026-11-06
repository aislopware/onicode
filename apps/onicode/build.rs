fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packaging/macos/Info-unbundled.plist");
        println!("cargo:rerun-if-changed={}", plist.display());
        // `cargo run` has no .app Info.plist. NSBundle also reads privacy
        // declarations from this Mach-O section, so source builds can request
        // microphone access without requiring a separate packaging step.
        println!(
            "cargo:rustc-link-arg-bin=onicode=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
