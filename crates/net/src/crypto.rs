//! A QUIC crypto provider that does no cryptography.
//!
//! Ported from axolotty's `axquic` `crypto.rs`, itself slopty's `slopty-net` (MIT).
//!
//! Every onicode peer is reached over Tailscale (`WireGuard`), which already encrypts and
//! authenticates the path; a second layer here would only cost latency and a pairing ceremony
//! (slopty measured it: control round trip 1.2 → 0.55 ms, connect to hello 30 → 11 ms). So
//! packets go in the clear: the packet keys copy, the header keys leave the header alone, and the
//! tag is zero bytes long.
//!
//! What is left of a handshake is the exchange QUIC cannot do without, the transport
//! parameters, in one round trip:
//!
//! ```text
//! client  Initial    HELLO(params)      ─▶
//!                                       ◀─  Initial    HELLO(params)   server
//!                                       ◀─  Handshake  FINISHED
//! client  Handshake  FINISHED           ─▶
//! ```
//!
//! `HELLO` is [`MAGIC`], a big-endian `u16` length and the parameters as QUIC encodes them;
//! `FINISHED` is one byte. The FINISHED messages exist because noq moves each side to the next
//! packet space only when it has handshake bytes to send there, and marks the connection
//! established when a Handshake packet arrives after the session stops handshaking. onicode runs
//! its own QUIC version ([`QUIC_VERSION`]), so a standard QUIC stack gets a version
//! negotiation instead of plaintext it would take for garbage.

use std::any::Any;
use std::sync::Arc;

use bytes::BytesMut;
use noq_proto::crypto::{
    self, CryptoError, ExportKeyingMaterialError, HandshakeTokenKey, HeaderKey, HmacKey, KeyPair,
    Keys, PacketKey, UnsupportedVersion,
};
use noq_proto::transport_parameters::TransportParameters;
use noq_proto::{ConnectError, ConnectionId, PathId, Side, TransportError, TransportErrorCode};

/// The QUIC version on the wire: "ONI1". Not a registered version and not of the reserved
/// `0x?a?a?a?a` greasing form.
pub const QUIC_VERSION: u32 = u32::from_be_bytes(*b"ONI1");

/// First bytes of a `HELLO`. The wire is not versioned here: the app hello on the control
/// stream carries onicode's wire generation once the link is up (`crate::hello`).
pub const MAGIC: &[u8; 8] = b"onicode\0";

/// The one byte of a `FINISHED`.
const FINISHED: u8 = 0xf1;

/// The largest transport-parameter block accepted (they are ~100 bytes in practice).
const MAX_PARAMS: usize = 4096;

/// Length of the retry integrity tag and of an HMAC signature.
const TAG_LEN: usize = 16;

/// The header-protection sample the plain header key claims, which nothing reads.
///
/// noq pads every packet to hold one after the packet number, so a PING is 29 bytes rather
/// than 13. A peer answers a packet for a connection it does not know with a stateless reset
/// only when that packet is at least 22 bytes (RFC 9000 section 10.3: the reset must be
/// shorter than what incited it and still look like a packet). Without the padding, the
/// keep-alive a peer sends to a restarted one drew no reset, and it learnt the peer was gone
/// only from its silence.
const SAMPLE: usize = 16;

/// The key of the connection-ID generator, the same in every onicode process: a restarted peer
/// takes the connection IDs it handed out before as its own, and answers them with a stateless
/// reset instead of dropping them as noise.
const CID_KEY: u64 = u64::from_be_bytes(*b"onicids\0");

/// A packet key and a header key that leave every byte as it is.
#[derive(Clone, Copy, Debug)]
struct Plain;

impl PacketKey for Plain {
    fn encrypt(&self, _path: PathId, _packet: u64, _buf: &mut [u8], _header_len: usize) {}

    fn decrypt(
        &self,
        _path: PathId,
        _packet: u64,
        _header: &[u8],
        _payload: &mut BytesMut,
    ) -> Result<(), CryptoError> {
        Ok(())
    }

    fn tag_len(&self) -> usize {
        0
    }

    fn confidentiality_limit(&self) -> u64 {
        u64::MAX
    }

    fn integrity_limit(&self) -> u64 {
        u64::MAX
    }
}

impl HeaderKey for Plain {
    fn decrypt(&self, _: usize, _: &mut [u8]) {}

    fn encrypt(&self, _: usize, _: &mut [u8]) {}

    fn sample_size(&self) -> usize {
        SAMPLE
    }
}

/// A full set of plain keys for one packet space.
fn keys() -> Keys {
    Keys {
        header: KeyPair {
            local: Box::new(Plain),
            remote: Box::new(Plain),
        },
        packet: KeyPair {
            local: Box::new(Plain),
            remote: Box::new(Plain),
        },
    }
}

/// The client half of the provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct Client;

impl crypto::ClientConfig for Client {
    fn start_session(
        &self,
        version: u32,
        _server_name: &str,
        params: &TransportParameters,
    ) -> Result<Box<dyn crypto::Session>, ConnectError> {
        if version != QUIC_VERSION {
            return Err(ConnectError::UnsupportedVersion);
        }
        Ok(Box::new(Session::new(Side::Client, params)))
    }
}

/// The server half of the provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct Server;

impl crypto::ServerConfig for Server {
    fn initial_keys(
        &self,
        version: u32,
        _dst_cid: ConnectionId,
    ) -> Result<Keys, UnsupportedVersion> {
        if version == QUIC_VERSION {
            Ok(keys())
        } else {
            Err(UnsupportedVersion)
        }
    }

    fn retry_tag(&self, _version: u32, _orig_dst_cid: ConnectionId, _packet: &[u8]) -> [u8; 16] {
        [0; TAG_LEN]
    }

    fn start_session(
        &self,
        _version: u32,
        params: &TransportParameters,
    ) -> Box<dyn crypto::Session> {
        Box::new(Session::new(Side::Server, params))
    }
}

/// Where a session stands. Each side sends `HELLO` then `FINISHED`; the server sends its
/// `HELLO` only after reading the client's, and the client its `FINISHED` only after reading
/// the server's.
///
/// That last order is TLS's, and noq depends on it. The server finishes on the client's
/// `FINISHED` and throws its Handshake keys away at once, so from then on it can neither resend
/// its own `FINISHED` nor answer a Handshake packet; the client, for its part, is connected only
/// by a Handshake packet from the server. A client that sent its `FINISHED` on the server's
/// `HELLO` alone, as this provider first did, finished the server while the server's `FINISHED`
/// could still be lost, and then waited out its dial for a packet that would never come (one
/// dial in seven at a fifth lost each way, in slopty's simulated network).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// Nothing sent yet.
    Start,
    /// Our `HELLO` is out; waiting for the peer's.
    AwaitHello,
    /// The peer's `HELLO` is in and ours is not (server) or its keys are not handed out
    /// (client).
    GotHello,
    /// Handshake keys handed out; `FINISHED` and the 1-RTT keys are next.
    HandshakeKeys,
    /// Our `FINISHED` is out; waiting for the peer's.
    AwaitFinished,
    /// Both `FINISHED`s exchanged.
    Done,
}

/// One connection's handshake.
struct Session {
    side: Side,
    /// Our transport parameters, encoded.
    local: Vec<u8>,
    /// The peer's, once its `HELLO` is in.
    peer: Option<TransportParameters>,
    /// Handshake bytes received and not yet consumed.
    inbox: Vec<u8>,
    stage: Stage,
    /// The peer's `FINISHED` arrived.
    finished: bool,
}

impl Session {
    fn new(side: Side, params: &TransportParameters) -> Self {
        let mut local = Vec::new();
        params.write(&mut local);
        Self {
            side,
            local,
            peer: None,
            inbox: Vec::new(),
            stage: Stage::Start,
            finished: false,
        }
    }

    fn write_hello(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(MAGIC);
        // `local` is ~100 bytes; a block over `u16::MAX` would be a noq bug, and one this
        // side refuses to read anyway.
        let len = u16::try_from(self.local.len()).unwrap_or(u16::MAX);
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&self.local);
    }

    /// Consume a whole `HELLO` from the inbox if one is there.
    fn take_hello(&mut self) -> Result<bool, TransportError> {
        let Some(head) = self.inbox.get(..MAGIC.len()) else {
            return Ok(false);
        };
        if head != MAGIC {
            return Err(violation("not an onicode handshake"));
        }
        let Some(&[hi, lo]) = self.inbox.get(MAGIC.len()..MAGIC.len().saturating_add(2)) else {
            return Ok(false);
        };
        let len = usize::from(u16::from_be_bytes([hi, lo]));
        if len > MAX_PARAMS {
            return Err(violation("transport parameters too long"));
        }
        let start = MAGIC.len().saturating_add(2);
        let end = start.saturating_add(len);
        let Some(mut block) = self.inbox.get(start..end) else {
            return Ok(false);
        };
        let params = TransportParameters::read(self.side, &mut block).map_err(|e| {
            TransportError::new(
                TransportErrorCode::TRANSPORT_PARAMETER_ERROR,
                format!("bad transport parameters: {e}"),
            )
        })?;
        self.peer = Some(params);
        self.inbox.drain(..end);
        Ok(true)
    }

    /// Consume a `FINISHED` from the inbox if one is there.
    fn take_finished(&mut self) -> Result<(), TransportError> {
        match self.inbox.first() {
            None => Ok(()),
            Some(&FINISHED) => {
                self.finished = true;
                self.inbox.remove(0);
                Ok(())
            }
            Some(_) => Err(violation("expected FINISHED")),
        }
    }
}

fn violation(why: &str) -> TransportError {
    TransportError::new(TransportErrorCode::PROTOCOL_VIOLATION, why.to_owned())
}

impl crypto::Session for Session {
    fn initial_keys(&self, _dst_cid: ConnectionId, _side: Side) -> Keys {
        keys()
    }

    fn handshake_data(&self) -> Option<Box<dyn Any>> {
        let data: Box<dyn Any> = Box::new(());
        self.peer.map(|_| data)
    }

    fn peer_identity(&self) -> Option<Box<dyn Any>> {
        None
    }

    fn early_crypto(&self) -> Option<(Box<dyn HeaderKey>, Box<dyn PacketKey>)> {
        None
    }

    fn early_data_accepted(&self) -> Option<bool> {
        None
    }

    fn is_handshaking(&self) -> bool {
        self.stage != Stage::Done
    }

    fn read_handshake(&mut self, buf: &[u8]) -> Result<bool, TransportError> {
        self.inbox.extend_from_slice(buf);
        if self.inbox.len() > MAX_PARAMS.saturating_add(MAGIC.len()).saturating_add(3) {
            return Err(violation("handshake too long"));
        }
        let mut ready = false;
        if self.peer.is_none() {
            if !self.take_hello()? {
                return Ok(false);
            }
            ready = true;
            if matches!(self.stage, Stage::Start | Stage::AwaitHello) {
                self.stage = Stage::GotHello;
            }
        }
        self.take_finished()?;
        if !self.inbox.is_empty() {
            return Err(violation("trailing handshake bytes"));
        }
        if self.finished && self.stage == Stage::AwaitFinished {
            self.stage = Stage::Done;
        }
        Ok(ready)
    }

    fn transport_parameters(&self) -> Result<Option<TransportParameters>, TransportError> {
        Ok(self.peer)
    }

    fn write_handshake(&mut self, buf: &mut Vec<u8>) -> Option<Keys> {
        match (self.side, self.stage) {
            (Side::Client, Stage::Start) => {
                self.write_hello(buf);
                self.stage = Stage::AwaitHello;
                None
            }
            (Side::Server, Stage::GotHello) => {
                self.write_hello(buf);
                self.stage = Stage::HandshakeKeys;
                Some(keys())
            }
            (Side::Client, Stage::GotHello) => {
                self.stage = Stage::HandshakeKeys;
                Some(keys())
            }
            // The server's FINISHED first: see `Stage`.
            (Side::Client, Stage::HandshakeKeys) if !self.finished => None,
            (_, Stage::HandshakeKeys) => {
                buf.push(FINISHED);
                self.stage = if self.finished {
                    Stage::Done
                } else {
                    Stage::AwaitFinished
                };
                Some(keys())
            }
            (_, Stage::Start | Stage::AwaitHello | Stage::AwaitFinished | Stage::Done) => None,
        }
    }

    fn next_1rtt_keys(&mut self) -> Option<KeyPair<Box<dyn PacketKey>>> {
        Some(KeyPair {
            local: Box::new(Plain),
            remote: Box::new(Plain),
        })
    }

    fn is_valid_retry(&self, _orig_dst_cid: ConnectionId, _header: &[u8], payload: &[u8]) -> bool {
        payload
            .len()
            .checked_sub(TAG_LEN)
            .and_then(|at| payload.get(at..))
            .is_some_and(|tag| tag == [0; TAG_LEN])
    }

    fn export_keying_material(
        &self,
        _output: &mut [u8],
        _label: &[u8],
        _context: &[u8],
    ) -> Result<(), ExportKeyingMaterialError> {
        Err(ExportKeyingMaterialError)
    }
}

/// A keyed hash in the shape of an HMAC, for stateless-reset and address-validation tokens.
///
/// Not a MAC an attacker cannot forge: the network is the trust boundary here, as for
/// everything else in this module. The key is the same in every onicode process so a restarted
/// peer answers the old connection's packets with a reset the client recognises, rather than
/// leaving it to time out.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keyed;

/// Fixed key mixed into [`Keyed`].
const KEYED: &[u8] = b"onicode keyed hash";

impl Keyed {
    /// SHA-256 over the fixed key, a block counter and the data: stable across builds and
    /// toolchains, so two peers on different versions agree on every token.
    fn tag(data: &[u8], out: &mut [u8]) {
        use sha2::Digest as _;
        for (block, chunk) in (0_u64..).zip(out.chunks_mut(32)) {
            let digest = sha2::Sha256::new()
                .chain_update(KEYED)
                .chain_update(block.to_le_bytes())
                .chain_update(data)
                .finalize();
            chunk.copy_from_slice(&digest[..chunk.len()]);
        }
    }
}

impl HmacKey for Keyed {
    fn sign(&self, data: &[u8], signature_out: &mut [u8]) {
        Self::tag(data, signature_out);
    }

    fn signature_len(&self) -> usize {
        TAG_LEN
    }

    fn verify(&self, data: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        let mut expected = [0; TAG_LEN];
        Self::tag(data, &mut expected);
        if signature == expected {
            Ok(())
        } else {
            Err(CryptoError)
        }
    }
}

impl HandshakeTokenKey for Keyed {
    fn seal(&self, token_nonce: u128, data: &mut Vec<u8>) -> Result<(), CryptoError> {
        let mut tag = [0; TAG_LEN];
        let mut input = token_nonce.to_le_bytes().to_vec();
        input.extend_from_slice(data);
        Self::tag(&input, &mut tag);
        data.extend_from_slice(&tag);
        Ok(())
    }

    fn open<'a>(&self, token_nonce: u128, data: &'a mut [u8]) -> Result<&'a [u8], CryptoError> {
        let at = data.len().checked_sub(TAG_LEN).ok_or(CryptoError)?;
        let (plain, tag) = data.split_at(at);
        let mut input = token_nonce.to_le_bytes().to_vec();
        input.extend_from_slice(plain);
        let mut expected = [0; TAG_LEN];
        Self::tag(&input, &mut expected);
        if tag != expected {
            return Err(CryptoError);
        }
        Ok(plain)
    }
}

/// The client side's QUIC config over this provider.
#[must_use]
pub fn client_config() -> noq::ClientConfig {
    let mut config = noq::ClientConfig::new(Arc::new(Client));
    config.version(QUIC_VERSION);
    config
}

/// The server side's QUIC config over this provider, without its transport config.
#[must_use]
pub fn server_config() -> noq::ServerConfig {
    let mut config = noq::ServerConfig::new(Arc::new(Server), Arc::new(Keyed));
    // Address-validation tokens let a returning client skip a check this server never makes.
    config.validation_token.sent(0);
    config
}

/// The endpoint config: onicode's QUIC version only, and the shared reset and connection-ID
/// keys.
#[must_use]
pub fn endpoint_config() -> noq::EndpointConfig {
    let mut config = noq::EndpointConfig::new(Arc::new(Keyed));
    config
        .supported_versions(vec![QUIC_VERSION])
        .cid_generator(Arc::new(|| {
            Box::new(noq_proto::HashedConnectionIdGenerator::from_key(CID_KEY))
        }));
    config
}

#[cfg(test)]
mod tests {
    use noq_proto::crypto::{ClientConfig as _, ServerConfig as _};

    use super::*;

    /// The protocol defaults but for a 30 s idle timeout (noq's own constructor is private,
    /// and an all-default block encodes to nothing).
    fn params() -> TransportParameters {
        let block = [0x01, 0x04, 0x80, 0x00, 0x75, 0x30];
        TransportParameters::read(Side::Client, &mut &block[..]).unwrap()
    }

    fn pair() -> (Box<dyn crypto::Session>, Box<dyn crypto::Session>) {
        let client = Client
            .start_session(QUIC_VERSION, "peer", &params())
            .unwrap();
        let server = Server.start_session(QUIC_VERSION, &params());
        (client, server)
    }

    #[test]
    fn a_handshake_exchanges_parameters_in_one_round_trip() {
        let (mut client, mut server) = pair();
        let mut hello = Vec::new();
        assert!(
            client.write_handshake(&mut hello).is_none(),
            "no keys before the server speaks"
        );
        assert!(hello.starts_with(MAGIC));
        assert!(client.is_handshaking() && server.is_handshaking());

        assert!(
            server.read_handshake(&hello).unwrap(),
            "the client's parameters are data"
        );
        assert!(server.transport_parameters().unwrap().is_some());
        let mut reply = Vec::new();
        assert!(
            server.write_handshake(&mut reply).is_some(),
            "handshake keys"
        );
        let mut fin = Vec::new();
        assert!(server.write_handshake(&mut fin).is_some(), "1-RTT keys");
        assert_eq!(fin, [FINISHED]);
        assert!(server.write_handshake(&mut Vec::new()).is_none());
        assert!(
            server.is_handshaking(),
            "the server waits for the client's FINISHED"
        );

        assert!(client.read_handshake(&reply).unwrap());
        assert!(client.read_handshake(&fin).is_ok_and(|ready| !ready));
        let mut out = Vec::new();
        assert!(client.write_handshake(&mut out).is_some(), "handshake keys");
        assert_eq!(out, b"");
        assert!(client.write_handshake(&mut out).is_some(), "1-RTT keys");
        assert_eq!(out, [FINISHED]);
        assert!(!client.is_handshaking());
        assert!(client.transport_parameters().unwrap().is_some());

        server.read_handshake(&out).unwrap();
        assert!(!server.is_handshaking());
        assert!(client.next_1rtt_keys().is_some() && server.next_1rtt_keys().is_some());
    }

    /// The server's HELLO arrived and its FINISHED was lost: the client has Handshake keys to
    /// read the resent FINISHED with, and sends its own only once that is in. Sent earlier, it
    /// would finish the server, which then drops the Handshake keys it would resend with.
    #[test]
    fn the_client_finishes_only_after_the_server_has() {
        let (mut client, mut server) = pair();
        let mut hello = Vec::new();
        client.write_handshake(&mut hello);
        server.read_handshake(&hello).unwrap();
        let (mut reply, mut fin) = (Vec::new(), Vec::new());
        server.write_handshake(&mut reply);
        server.write_handshake(&mut fin);

        assert!(client.read_handshake(&reply).unwrap());
        let mut out = Vec::new();
        assert!(client.write_handshake(&mut out).is_some(), "handshake keys");
        assert!(
            client.write_handshake(&mut out).is_none(),
            "no 1-RTT keys yet"
        );
        assert!(out.is_empty(), "no FINISHED before the server's: {out:?}");
        assert!(client.is_handshaking());

        client.read_handshake(&fin).unwrap();
        assert!(client.write_handshake(&mut out).is_some(), "1-RTT keys");
        assert_eq!(out, [FINISHED]);
        assert!(!client.is_handshaking());
        server.read_handshake(&out).unwrap();
        assert!(!server.is_handshaking());
    }

    #[test]
    fn a_hello_split_across_frames_is_read_whole() {
        let (mut client, mut server) = pair();
        let mut hello = Vec::new();
        client.write_handshake(&mut hello);
        let (a, b) = hello.split_at(5);
        assert!(
            !server.read_handshake(a).unwrap(),
            "half a magic is not yet data"
        );
        let (b1, b2) = b.split_at(6);
        assert!(
            !server.read_handshake(b1).unwrap(),
            "the length but not the block"
        );
        assert!(server.read_handshake(b2).unwrap());
    }

    #[test]
    fn bad_handshake_bytes_are_an_error_not_a_panic() {
        let tls = [
            0x16, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01, 0xfc, 0x03, 0x03,
        ];
        let (_client, mut server) = pair();
        assert!(
            server.read_handshake(&tls).is_err(),
            "a TLS ClientHello is not ours"
        );

        let mut bad_params = MAGIC.to_vec();
        bad_params.extend_from_slice(&3_u16.to_be_bytes());
        bad_params.extend_from_slice(&[0x40, 0xff, 0x09]);
        let (_client, mut server) = pair();
        let err = server.read_handshake(&bad_params).unwrap_err();
        assert_eq!(err.code, TransportErrorCode::TRANSPORT_PARAMETER_ERROR);

        let mut long = MAGIC.to_vec();
        long.extend_from_slice(&u16::MAX.to_be_bytes());
        let (_client, mut server) = pair();
        assert!(
            server.read_handshake(&long).is_err(),
            "a block past the cap"
        );

        let (mut client, mut server) = pair();
        let mut hello = Vec::new();
        client.write_handshake(&mut hello);
        hello.push(0x00);
        assert!(
            server.read_handshake(&hello).is_err(),
            "junk after the hello"
        );

        let (_client, mut server) = pair();
        assert!(
            server.read_handshake(&[]).is_ok_and(|ready| !ready),
            "nothing is not an error"
        );
    }

    #[test]
    fn keys_leave_bytes_alone_and_never_run_out() {
        let k = Plain;
        let mut buf = BytesMut::from(&b"payload"[..]);
        PacketKey::decrypt(&k, PathId::ZERO, 7, b"hdr", &mut buf).unwrap();
        assert_eq!(&*buf, b"payload");
        assert_eq!(k.tag_len(), 0);
        assert_eq!(k.sample_size(), SAMPLE);
        assert_eq!(k.confidentiality_limit(), u64::MAX);
        assert_eq!(k.integrity_limit(), u64::MAX);
        assert!(
            Server.initial_keys(1, ConnectionId::new(&[1; 8])).is_err(),
            "QUIC v1 is refused"
        );
        assert!(Client.start_session(1, "peer", &params()).is_err());
    }

    #[test]
    fn a_retry_carries_a_tag_the_client_accepts_and_nothing_else_leaks() {
        let cid = ConnectionId::new(&[3; 8]);
        let tag = Server.retry_tag(QUIC_VERSION, cid, b"packet");
        let (client, _server) = pair();
        let mut payload = b"token".to_vec();
        payload.extend_from_slice(&tag);
        assert!(client.is_valid_retry(cid, b"header", &payload));
        assert!(!client.is_valid_retry(cid, b"header", b"short"));
        payload.push(1);
        assert!(!client.is_valid_retry(cid, b"header", &payload));
        assert!(
            client
                .export_keying_material(&mut [0; 32], b"l", b"c")
                .is_err()
        );
    }

    #[test]
    fn keyed_tokens_open_what_they_sealed_and_nothing_altered() {
        let key = Keyed;
        let mut token = b"address + time".to_vec();
        key.seal(42, &mut token).unwrap();
        assert_eq!(key.open(42, &mut token.clone()).unwrap(), b"address + time");
        assert!(key.open(43, &mut token.clone()).is_err(), "another nonce");
        let mut flipped = token.clone();
        flipped[0] ^= 1;
        key.open(42, &mut flipped).unwrap_err();
        assert!(key.open(42, &mut [0; 3]).is_err(), "shorter than a tag");

        let mut sig = [0; TAG_LEN];
        key.sign(b"cid", &mut sig);
        key.verify(b"cid", &sig).unwrap();
        assert!(key.verify(b"cie", &sig).is_err());
    }
}
