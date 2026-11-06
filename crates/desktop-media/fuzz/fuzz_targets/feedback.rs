//! The `feedback` target: [`onicode_desktop_media_fuzz::feedback::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

onicode_desktop_media_fuzz::target!(|data| onicode_desktop_media_fuzz::feedback::run(data));
