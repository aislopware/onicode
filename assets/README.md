# Brand icons

The two Icon Composer projects are the source of truth for full application icons:

- `dev/app-icon.icon`
- `prod/app-icon.icon`

Each project is a solid brand-red fill and one `mochi.svg` layer. The OS adds the shape and, on iOS and macOS 26, the Liquid Glass: the iOS app and the packaged macOS app ship the project itself (macOS through electron-builder, which needs Xcode 26's `actool`), so never bake a rim or mask into the artwork.

Run `vp run icons:export` from the repository root to regenerate the tracked iOS, Linux, and Windows assets. Run `vp run icons:check` to verify that the generated assets and public copies match their sources without changing files.

Exporting requires Icon Composer 2 or newer on macOS. The script selects the newest compatible exporter from Xcode or a standalone Icon Composer installation and pins design generation 26. Set `ICON_COMPOSER_TOOL` to the full path of `Icon Composer.app/Contents/Executables/ictool` to override automatic discovery.

## macOS exports

The packaged app compiles the Icon Composer project. The macOS PNGs below are only the dev app's Dock icon and the permission helper's picture.

Icon Composer's command-line exporter does not expose the `macOS pre-Tahoe` preset. A plain command-line `macOS` export is full bleed and is not suitable for the desktop app, so the export script intentionally leaves the tracked macOS PNGs unchanged and prints a reminder after every run.

After changing an Icon Composer project, open it in Icon Composer and export the macOS PNG with exactly these settings:

- Platform: `macOS pre-Tahoe`
- Appearance: `Default`
- Size: `1024pt`
- Scale: `1×`

Save the two exports to:

- `dev/app-icon.icon` -> `dev/blueprint-macos-1024.png`
- `prod/app-icon.icon` -> `prod/black-macos-1024.png`

The result must be a 1024×1024 PNG with the classic macOS safe area: the opaque icon body is 824×824, inset 100 pixels on every side, with only the native Icon Composer shadow extending into the surrounding transparent canvas.

To have Codex perform the native exports, paste this prompt into a task opened at the repository root:

```text
Use [@Computer](plugin://computer-use@openai-bundled) and the Icon Composer app to export the two macOS app icons in this repository.

For each project below, use Platform: macOS pre-Tahoe, Appearance: Default, Size: 1024pt, and Scale: 1×, then save the PNG to the exact destination:

- assets/dev/app-icon.icon -> assets/dev/blueprint-macos-1024.png
- assets/prod/app-icon.icon -> assets/prod/black-macos-1024.png

Do not resize, composite, or otherwise post-process the exported PNGs.

Verify every result is 1024×1024 and has the classic macOS safe area: an 824×824 opaque body inset 100px on every side, with only Icon Composer's native shadow extending beyond it.
```

Do not edit the generated PNG or ICO files directly.

## Bare mochi artwork

`vp run icons:export:mochi` renders the mochi layer without anything the OS would add:

- `apps/mobile/assets/android-icon-foreground.png`: the adaptive icon foreground, inside the
  central 72dp Android keeps of its 108dp canvas. The red is the adaptive background colour.
- `apps/mobile/assets/splash-icon.png`: the iOS and Android splash image on brand red, inside the
  central two thirds of the 288dp canvas that Android 12+ masks.
- The `*-splash-logo-180.png` files and `apps/web/public/splash-logo.png`: a flat red tile for the
  places the app shows its own icon, rounded by the UI.

Rerun it after changing `mochi.svg` or the fill. `android-icon-mark.png` remains a flat silhouette
for Android's monochrome themed icon.
