//! A ⌘ or ⌃ chord by its character, not its position.
//!
//! Keys go by position, which the host's layout reads as the client's only once the host is
//! under the client's input source. A shortcut is meant by its character, though: ⌘Z on a German
//! client's keyboard is Z's key there, Y's position on a US one. So while the host is not
//! under the client's source, the client names a chord's character
//! ([`onicode_desktop_proto::screen::ScreenInput::Key::chord`]), and the host presses the key that
//! types it under its own layout ([`ChordTable`]), else the position the client sent
//! (`docs/decisions/input.md`, "A shortcut goes by its character").
//!
//! The table is built once per layout, where the platform's layout data may be read, and shared
//! by every stream ([`KeyLayout`]); a press then costs a read lock and a hash lookup, with no
//! allocation.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;

/// The key that types each character under one keyboard layout, by its virtual key code.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ChordTable {
    places: HashMap<char, u16>,
}

impl ChordTable {
    /// The table of a layout whose keys type `keys`: each virtual key code with what it types
    /// with ⌘ held, and with ⌘ and Shift (`crate::input_source::LayoutKey`). A
    /// character two keys type goes to the one that types it without Shift, then to the lower
    /// code (the main row before the keypad).
    pub fn from_keys(keys: impl IntoIterator<Item = (u16, Option<char>, Option<char>)>) -> Self {
        let mut keys: Vec<(u16, Option<char>, Option<char>)> = keys.into_iter().collect();
        keys.sort_by_key(|(vk, ..)| *vk);
        let mut places = HashMap::new();
        for (vk, base, _) in &keys {
            if let Some(c) = base {
                places.entry(lowered(*c)).or_insert(*vk);
            }
        }
        for (vk, _, shifted) in &keys {
            if let Some(c) = shifted {
                places.entry(lowered(*c)).or_insert(*vk);
            }
        }
        Self { places }
    }

    /// The virtual key code of the key that types `c`, lowercased, under this layout.
    #[must_use]
    pub fn place(&self, c: char) -> Option<u16> {
        self.places.get(&lowered(c)).copied()
    }
}

/// `c` lowercased, when that is one character.
fn lowered(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

/// The host's current layout's [`ChordTable`], shared by every stream; none until the layout
/// is read, and on a platform with none to read.
#[derive(Clone, Debug, Default)]
pub struct KeyLayout(Arc<RwLock<Option<Arc<ChordTable>>>>);

/// This process's keyboard layout ([`KeyLayout::system`]).
static SYSTEM: LazyLock<KeyLayout> = LazyLock::new(KeyLayout::default);

impl KeyLayout {
    /// This process's: the one [`crate::sources::Sources`] reads as the host's layout changes.
    #[must_use]
    pub fn system() -> Self {
        SYSTEM.clone()
    }

    /// The layout is now `table`'s.
    pub fn set(&self, table: ChordTable) {
        *self.0.write() = Some(Arc::new(table));
    }

    /// The virtual key code of the key that types `chord` (one character) under the layout;
    /// `None` for no such key, several characters, or no layout read.
    #[must_use]
    pub fn place(&self, chord: &str) -> Option<u16> {
        let mut chars = chord.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return None;
        };
        self.0.read().as_ref()?.place(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `kVK_ANSI_*` codes of the positions the fixtures use.
    const A: u16 = 0x00;
    const Q: u16 = 0x0c;
    const W: u16 = 0x0d;
    const Y: u16 = 0x10;
    const Z: u16 = 0x06;
    const ONE: u16 = 0x12;
    const KEYPAD_ONE: u16 = 0x53;

    /// French AZERTY, as far as the fixture goes: `a` on Q's place, `q` on A's, `z` on W's; `1`
    /// shifted on the row, and bare on the keypad.
    fn azerty() -> ChordTable {
        ChordTable::from_keys([
            (A, Some('q'), Some('Q')),
            (Q, Some('a'), Some('A')),
            (W, Some('z'), Some('Z')),
            (ONE, Some('&'), Some('1')),
            (KEYPAD_ONE, Some('1'), Some('1')),
        ])
    }

    /// German QWERTZ, as far as the fixture goes: `z` on Y's place, `y` on Z's.
    fn qwertz() -> ChordTable {
        ChordTable::from_keys([(Y, Some('z'), Some('Z')), (Z, Some('y'), Some('Y'))])
    }

    /// A chord's character is pressed where the host's layout types it: AZERTY's `a` on Q's
    /// place and `q` on A's, QWERTZ's `z` on Y's; an uppercase name reads as its letter.
    #[test]
    fn a_chord_goes_where_the_layout_types_its_character() {
        let azerty = azerty();
        assert_eq!(azerty.place('a'), Some(Q));
        assert_eq!(azerty.place('q'), Some(A));
        assert_eq!(azerty.place('Z'), Some(W));
        assert_eq!(qwertz().place('z'), Some(Y));
        assert_eq!(
            qwertz().place('x'),
            None,
            "a character the layout has no key for"
        );
    }

    /// A character typed bare beats one typed with Shift, wherever its key is: `1` is the
    /// keypad's on AZERTY, where the row types it only shifted.
    #[test]
    fn a_bare_character_beats_a_shifted_one() {
        assert_eq!(azerty().place('1'), Some(KEYPAD_ONE));
        assert_eq!(azerty().place('&'), Some(ONE));
    }

    /// The shared layout answers only once read, and only for one character.
    #[test]
    fn the_shared_layout_answers_one_character_once_read() {
        let layout = KeyLayout::default();
        assert_eq!(layout.place("z"), None, "nothing read yet");
        layout.set(qwertz());
        assert_eq!(layout.place("z"), Some(Y));
        assert_eq!(layout.place("zz"), None);
        assert_eq!(layout.place(""), None);
    }
}
