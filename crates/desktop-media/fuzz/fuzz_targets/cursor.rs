//! The `cursor` target: [`onicode_desktop_media_fuzz::datagram::cursor`].

#![cfg_attr(feature = "libfuzzer", no_main)]

onicode_desktop_media_fuzz::target!(|data| onicode_desktop_media_fuzz::datagram::cursor(data));
