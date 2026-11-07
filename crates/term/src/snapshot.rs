//! libghostty's snapshot framing, read only far enough to split a snapshot at its READY record.
//!
//! A snapshot is a 10-byte envelope (`GHOSTSNP` and a `u16` version) and then records of
//! `tag u16 | payload_len u32 | crc32c u32 | payload`, all little-endian. Everything through READY
//! paints the screen; the history after it is prepended as it decodes. The engine sends the two
//! parts apart, so a viewer far away paints before a long scrollback arrives.

const MAGIC: &[u8; 8] = b"GHOSTSNP";
const ENVELOPE: usize = 10;
const HEADER: usize = 10;
/// `record.zig`: READY ends the renderable prefix.
const TAG_READY: u16 = 5;
/// `record.zig`: FINISH ends the snapshot.
const TAG_FINISH: u16 = 6;

/// The length of the snapshot's prefix through its READY record, or `None` for bytes that are not
/// a snapshot or end before READY.
#[must_use]
pub fn ready_len(snapshot: &[u8]) -> Option<usize> {
    if snapshot.get(..MAGIC.len())? != MAGIC {
        return None;
    }
    let mut at = ENVELOPE;
    loop {
        let header = snapshot.get(at..at.checked_add(HEADER)?)?;
        let tag = u16::from_le_bytes([*header.first()?, *header.get(1)?]);
        let len = u32::from_le_bytes(header.get(2..6)?.try_into().ok()?);
        let end = at
            .checked_add(HEADER)?
            .checked_add(usize::try_from(len).ok()?)?;
        if end > snapshot.len() {
            return None;
        }
        match tag {
            TAG_READY => return Some(end),
            TAG_FINISH => return None,
            _ => at = end,
        }
    }
}

/// A snapshot as its renderable prefix and the history after it. Bytes that cannot be split stay
/// whole in the prefix, with no history: they decode the same either way.
#[must_use]
pub fn split(mut snapshot: Vec<u8>) -> (Vec<u8>, Vec<u8>) {
    match ready_len(&snapshot) {
        Some(ready) => {
            let history = snapshot.split_off(ready);
            (snapshot, history)
        }
        None => (snapshot, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(tag: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = tag.to_le_bytes().to_vec();
        out.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
        out.extend([0; 4]);
        out.extend(payload);
        out
    }

    #[test]
    fn the_prefix_ends_after_ready() {
        let mut snapshot = MAGIC.to_vec();
        snapshot.extend(1u16.to_le_bytes());
        snapshot.extend(record(1, b"terminal"));
        snapshot.extend(record(4, b""));
        snapshot.extend(record(TAG_READY, b""));
        let ready = snapshot.len();
        snapshot.extend(record(7, b"history"));
        snapshot.extend(record(TAG_FINISH, b""));
        assert_eq!(ready_len(&snapshot), Some(ready));
        let (prefix, history) = split(snapshot.clone());
        assert_eq!([prefix, history].concat(), snapshot);
    }

    #[test]
    fn truncated_or_foreign_bytes_have_no_prefix() {
        assert_eq!(ready_len(b"not a snapshot"), None);
        let mut snapshot = MAGIC.to_vec();
        snapshot.extend(1u16.to_le_bytes());
        snapshot.extend(record(1, b"terminal"));
        assert_eq!(ready_len(&snapshot), None, "no READY yet");
        snapshot.extend(&record(TAG_READY, b"")[..4]);
        assert_eq!(ready_len(&snapshot), None, "READY cut short");
        assert_eq!(split(snapshot.clone()), (snapshot, Vec::new()));
    }
}
