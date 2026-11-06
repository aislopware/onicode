//! The `reassemble` target: [`onicode_desktop_media_fuzz::reassemble::run`].

#![cfg_attr(feature = "libfuzzer", no_main)]

onicode_desktop_media_fuzz::target!(|data| onicode_desktop_media_fuzz::reassemble::run(data));
