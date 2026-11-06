//! Names for "keep both", as Finder makes them: `report.pdf` becomes `report 2.pdf`, then
//! `report 3.pdf`; `report 2.pdf` copied again becomes `report 3.pdf`. A leading dot is part of
//! the name, not an extension (`.env 2`).

/// The first free name after `name`: its stem with ` N` added, N from 2 (or one past the
/// number it already ends in), and its extension kept. `taken` says whether a name is in use.
#[must_use]
pub fn keep_both(name: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let (stem, ext) = split(name);
    let (base, first) = numbered(stem);
    let mut n = first;
    loop {
        let candidate = format!("{base} {n}{ext}");
        if !taken(&candidate) {
            return candidate;
        }
        match n.checked_add(1) {
            Some(next) => n = next,
            None => return format!("{base} {}{ext}", uuid::Uuid::new_v4().simple()),
        }
    }
}

/// `name` as stem and extension (with its dot); a leading dot starts no extension.
fn split(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => name.split_at(dot),
        _ => (name, ""),
    }
}

/// `stem` without a trailing ` N` (N ≥ 2), and the number to try first.
fn numbered(stem: &str) -> (&str, u64) {
    if let Some((base, digits)) = stem.rsplit_once(' ')
        && !base.is_empty()
        && !digits.is_empty()
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
        && let Ok(n) = digits.parse::<u64>()
        && n >= 2
    {
        return (base, n.saturating_add(1));
    }
    (stem, 2)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::*;

    fn free(name: &str, taken: &[&str]) -> String {
        let taken: HashSet<&str> = taken.iter().copied().collect();
        keep_both(name, &|c| taken.contains(c))
    }

    #[test]
    fn names_follow_finder() {
        assert_eq!(free("report.pdf", &["report.pdf"]), "report 2.pdf");
        assert_eq!(free("report.pdf", &["report 2.pdf"]), "report 3.pdf");
        assert_eq!(free("report 2.pdf", &[]), "report 3.pdf");
        assert_eq!(free("Photos", &[]), "Photos 2");
        assert_eq!(free("App.app", &[]), "App 2.app");
        assert_eq!(free(".env", &[]), ".env 2");
        assert_eq!(free("archive.tar.gz", &[]), "archive.tar 2.gz");
        assert_eq!(free("v 1", &[]), "v 1 2");
        assert_eq!(free("trailing.", &[]), "trailing. 2");
    }

    proptest! {
        /// The name is free, differs from the original, keeps its extension and is the same
        /// for the same inputs.
        #[test]
        fn a_kept_name_is_free_and_keeps_its_extension(
            stem in "[a-zA-Z0-9 ._-]{1,12}",
            ext in proptest::option::of("[a-z]{1,4}"),
            taken_numbers in proptest::collection::btree_set(2_u64..12, 0..8),
        ) {
            let name = match &ext {
                Some(ext) => format!("{stem}.{ext}"),
                None => stem.clone(),
            };
            let (base, _) = numbered(split(&name).0);
            let (_, kept_ext) = split(&name);
            let taken: HashSet<String> = taken_numbers
                .iter()
                .map(|n| format!("{base} {n}{kept_ext}"))
                .chain(std::iter::once(name.clone()))
                .collect();
            let got = keep_both(&name, &|c| taken.contains(c));
            prop_assert!(!taken.contains(&got), "{} is taken", got);
            prop_assert_ne!(&got, &name);
            prop_assert!(got.ends_with(kept_ext));
            prop_assert!(got.starts_with(base));
            prop_assert_eq!(&got, &keep_both(&name, &|c| taken.contains(c)));
        }
    }
}
