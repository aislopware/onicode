//! Standard base64 (padded) for bulk payloads: image attachments, file contents and upload
//! chunks. Every target is Apple silicon, which always has NEON, where base64's NEON engine
//! encodes about 2.4× and decodes about 2.1× as fast as its scalar one (8 MB, M-series). Both
//! read and write the same text as the scalar engine, which the tests hold it to.

pub const STANDARD: base64::engine::Neon =
    base64::engine::Neon::standard(base64::engine::general_purpose::PAD);

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use proptest::prelude::*;

    proptest! {
        /// The engine agrees with the scalar one byte for byte, both ways.
        #[test]
        fn matches_the_scalar_engine(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let scalar = base64::engine::general_purpose::STANDARD;
            let text = super::STANDARD.encode(&bytes);
            prop_assert_eq!(&text, &scalar.encode(&bytes));
            prop_assert_eq!(super::STANDARD.decode(&text).unwrap(), bytes);
        }

        /// What the scalar engine rejects, this one rejects too.
        #[test]
        fn rejects_what_the_scalar_engine_rejects(text in "[A-Za-z0-9+/=_\\- ]{0,256}") {
            let scalar = base64::engine::general_purpose::STANDARD;
            prop_assert_eq!(super::STANDARD.decode(&text).ok(), scalar.decode(&text).ok());
        }
    }
}
