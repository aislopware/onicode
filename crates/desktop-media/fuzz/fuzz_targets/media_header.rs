//! The `media_header` target: [`onicode_desktop_media_fuzz::datagram::media_header`].

#![cfg_attr(feature = "libfuzzer", no_main)]

onicode_desktop_media_fuzz::target!(|data| onicode_desktop_media_fuzz::datagram::media_header(
    data
));
