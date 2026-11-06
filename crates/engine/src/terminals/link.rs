//! Terminal streams: the protocol and both ends live in `onicode-term-link` (the phone speaks
//! it too); this is the host's side over this engine's PTYs.

pub use onicode_term_link::link::*;

use super::Terminals;

impl HostTerminals for Terminals {
    fn subscribe(
        &self,
        terminal_id: &str,
        after_seq: u64,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<onicode_proto::TerminalEvent>, String> {
        Terminals::subscribe(self, terminal_id, Some(after_seq)).map_err(|e| e.to_string())
    }

    fn write_sequenced(
        &self,
        terminal_id: &str,
        client: &str,
        seq: u64,
        bytes: &[u8],
    ) -> Result<bool, String> {
        Terminals::write_sequenced(self, terminal_id, client, seq, bytes).map_err(|e| e.to_string())
    }

    fn write_next(
        &self,
        terminal_id: &str,
        client: &str,
        seq: u64,
        bytes: &[u8],
    ) -> Result<bool, String> {
        Terminals::write_next(self, terminal_id, client, seq, bytes).map_err(|e| e.to_string())
    }

    fn resize(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        Terminals::resize(self, terminal_id, cols, rows).map_err(|e| e.to_string())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replayed_input_is_applied_once_per_client_and_a_copy_only_in_its_turn() {
        let dir = tempfile::tempdir().unwrap();
        let terminals = Terminals::new();
        let session = terminals
            .open_with_shell(dir.path().to_str().unwrap(), 80, 24, Some("/bin/sh"))
            .unwrap();
        let id = &session.id;
        assert!(terminals.write_sequenced(id, "a", 1, b"").unwrap());
        assert!(terminals.write_sequenced(id, "a", 2, b"").unwrap());
        assert!(
            !terminals.write_sequenced(id, "a", 1, b"").unwrap(),
            "replay"
        );
        assert!(
            !terminals.write_sequenced(id, "a", 2, b"").unwrap(),
            "replay"
        );
        assert!(terminals.write_sequenced(id, "a", 3, b"").unwrap());
        assert!(
            terminals.write_sequenced(id, "b", 1, b"").unwrap(),
            "each viewer numbers its own input"
        );
        assert!(
            !terminals.write_next(id, "a", 5, b"").unwrap(),
            "a copy ahead of input still on its stream waits for it"
        );
        assert!(!terminals.write_next(id, "a", 3, b"").unwrap(), "a repeat");
        assert!(
            terminals.write_next(id, "a", 4, b"").unwrap(),
            "the next one"
        );
        assert!(
            !terminals.write_sequenced(id, "a", 4, b"").unwrap(),
            "its stream copy"
        );
        terminals.close(id).unwrap();
    }
}
