//! Committed text cut to what one key event carries.
//!
//! `CGEventKeyboardSetUnicodeString` takes at most 20 UTF-16 units per event: Chrome Remote
//! Desktop found longer strings cut there (`remoting/host/input_injector_mac.cc`). So text goes
//! in pieces of at most [`MOST_UNITS`], cut only between composed character sequences (what
//! CoreFoundation says a person sees as one character: a base letter and its marks, a
//! surrogate pair, an emoji with its joiners), never inside one, since a mark or half a pair
//! arriving alone types something else.

use objc2_core_foundation::{CFIndex, CFString};

/// The most UTF-16 units one event carries.
pub const MOST_UNITS: usize = 20;

/// `text` in pieces of at most [`MOST_UNITS`] UTF-16 units.
///
/// Pieces are cut between composed character sequences. A single sequence longer than that (a
/// long run of combining marks) is cut between code points, as nothing better exists.
#[must_use]
pub fn chunks(text: &str) -> Vec<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let cf = CFString::from_str(text);
    let mut pieces = Vec::new();
    let mut start = 0_usize;
    let mut at = 0_usize;
    while at < units.len() {
        let end = sequence_end(&cf, at).clamp(at.saturating_add(1), units.len());
        if end.saturating_sub(start) > MOST_UNITS && at > start {
            pieces.push(piece(&units, start, at));
            start = at;
        }
        at = end;
        while at.saturating_sub(start) > MOST_UNITS {
            let mut cut = start.saturating_add(MOST_UNITS);
            // Not between the halves of a surrogate pair.
            if units.get(cut).is_some_and(|u| (0xdc00..0xe000).contains(u)) {
                cut = cut.saturating_sub(1);
            }
            pieces.push(piece(&units, start, cut));
            start = cut;
        }
    }
    if start < units.len() {
        pieces.push(piece(&units, start, units.len()));
    }
    pieces
}

/// Where the composed character sequence holding UTF-16 unit `at` ends.
fn sequence_end(cf: &CFString, at: usize) -> usize {
    let Ok(index) = CFIndex::try_from(at) else {
        return at.saturating_add(1);
    };
    // SAFETY: `index` is inside the string: `at` is below the length of the UTF-16 units it
    // was made from, which is the CFString's length.
    let range = unsafe { cf.range_of_composed_characters_at_index(index) };
    let end = range.location.saturating_add(range.length);
    usize::try_from(end).unwrap_or_else(|_negative| at.saturating_add(1))
}

fn piece(units: &[u16], from: usize, to: usize) -> String {
    String::from_utf16_lossy(units.get(from..to).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::{MOST_UNITS, chunks};

    fn units(s: &str) -> usize {
        s.encode_utf16().count()
    }

    /// Short text goes whole; long text in pieces of at most 20 units that join back to it;
    /// no piece starts with a combining mark or the second half of a surrogate pair.
    #[test]
    fn text_goes_in_graphemes_of_at_most_20_units() {
        assert_eq!(chunks("é"), ["é"]);
        assert_eq!(chunks("日本語"), ["日本語"]);
        assert_eq!(chunks(""), Vec::<String>::new());

        // Vietnamese typed decomposed: each vowel carries two combining marks.
        let decomposed = "tie\u{302}\u{301}ng Vie\u{323}\u{302}t ".repeat(4);
        let emoji = "👩‍👩‍👧‍👦🇻🇳".repeat(3);
        for text in [
            decomposed.as_str(),
            emoji.as_str(),
            &"a".repeat(45),
            &"𝄞".repeat(15),
        ] {
            let pieces = chunks(text);
            assert_eq!(pieces.concat(), text);
            for piece in &pieces {
                assert!(
                    units(piece) <= MOST_UNITS,
                    "{piece:?} is {} units",
                    units(piece)
                );
                let first = piece.chars().next().map_or(0, u32::from);
                assert!(!(0x300..0x370).contains(&first), "a mark leads {piece:?}");
                assert!(!piece.contains('\u{fffd}'), "a pair was split in {piece:?}");
            }
        }
        // The family emoji is 11 units: a piece holds one, never a half.
        assert!(
            chunks(&emoji)
                .iter()
                .all(|p| p.starts_with('👩') || p.starts_with('🇻'))
        );
        // Twenty units exactly is one piece.
        assert_eq!(chunks(&"a".repeat(20)).len(), 1);
        assert_eq!(chunks(&"a".repeat(21)).len(), 2);
    }
}
