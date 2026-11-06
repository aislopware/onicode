# The office Macs' CI image

The office Macs run onicode's macOS and iOS lanes in one tart image, `onicode/macos-27-ci` on Zot on office-1
(`scripts/ci/image/`). It holds only what changes with a pin: macOS and Xcode 27.2, the Metal toolchain, the iOS 27.2
runtime with one settled simulator, zig, cargo-nextest, cargo-sweep and rustup itself. Rust toolchains, the registry,
the sources and the target live on the runners' persistent `/Volumes/CICache` instead, so a new stable or a new commit
never needs a new image, and a new image keeps the warm cache. The image is on Zot, not Harbor, because the runners
share the office LAN with office-1 while Harbor sits in IDC behind the office's slower internet link. Zot ends any
request after 60 s, so pushes go in 100 MB chunks and runners pull three layers at once.

The base's disk is ASIF, and the guest's TRIM reaches it, so files deleted in the guest leave the pushed image.
Dropping the base's tvOS, watchOS, visionOS and second iOS runtimes, Flutter and the Android SDK takes the image
from 77 to 49 GB compressed. That matters because a Mac's first pull runs at its link's speed, about 11 MB/s on the
Mac mini. Stacked images (`tart clone --stacked`) would send only the layers that changed, but the runners' tart
would need upgrading first.
