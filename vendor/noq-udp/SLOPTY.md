# noq-udp, vendored with three patches

onicode took this copy from axolotty's `axquic` on 2026-11-06, unchanged; the patches and their
measurements are slopty's.

The published `noq-udp` 1.4.0 (crates.io, upstream tag `noq-udp-v1.4.0`, commit `34f75c49f` of
<https://github.com/n0-computer/noq>) with these commits on top. On 1.3.0 there were five: 1.4.0
has three of them ("The sync to 1.4.0", below), and the first's test stays as patch 3.

1. `feat(udp): Report how many datagrams a send took`

   Port of the noq-udp half of quinn-rs/quinn#2748 (approved, still open), kept additive so noq
   itself builds unchanged. `sendmsg_x` can take fewer messages than it is given, and returns
   how many it took; noq ignores the count, so the rest of a batch is dropped without a
   word. `UdpSocketState::try_send_partial` returns that count, `Transmit::datagram_count` and
   `Transmit::advance` give what is left without copying, and the old `send` and `try_send` go
   on after a partial send until the socket has no room, when the rest is dropped as a full
   buffer would drop it. A transmit longer than one batch now goes a batch at a time instead of
   tripping a `debug_assert`, and the fallback when `sendmsg_x` cannot be resolved sends one
   datagram, not the whole buffer as one. onicode's socket (`onicode_net::udp`, from slopty) keeps the part
   not yet sent across a wait for room, which is quinn's connection-side half. 1.4.0
   deprecates `send` (#823, `31032ff1`); it goes on after a partial send as `try_send` does.
   Tests: `apple_fast_datapath_reports_a_partial_send`, `a_whole_send_reports_every_datagram`.

2. `feat(udp): Enable the Apple fast path only where the OS has it`

   `UdpSocketState::try_enable_apple_fast_path` resolves `sendmsg_x` and `recvmsg_x` first and
   enables the path only when both exist, which is the one condition the `unsafe`
   `set_apple_fast_path` leaves to its caller. noq's own runtime never enables the path, and
   onicode's transport crate (`onicode-net`) forbids `unsafe`.

3. `test(udp): No datagram size blocks for good at the send buffer's edge`

   A non-blocking `sendmsg` or `sendmsg_x` with a `cmsg` whose payload sits at or just under
   `SO_SNDBUF` returns `EWOULDBLOCK` for good on macOS (Apple FB23671230), and the socket never
   becomes writable for it. 1.4.0 has quinn's fix (quinn-rs/quinn#2727, as `9c42264c` and
   `49477cf0`), the same code this crate carried as its patch 1 since July: the socket state
   raises `SO_SNDBUF` to `65535 + cmsg::LEN` when it is made and whenever it is resized, and a
   send that would still land in the bug's region fails with `EMSGSIZE` instead, which the
   caller treats as a lost datagram. `no_datagram_size_blocks_for_good_at_the_send_buffer_edge`
   sends every size across the edge of the effective buffer, with an ECN `cmsg`, on both paths.
   Without the per-send check it fails with `EWOULDBLOCK` at 65 616 bytes against a 65 631-byte
   buffer. Upstream's `sndbuf_cmsg_boundary` shows the kernel's bug with a raw `sendmsg`, and
   then, like `sndbuf_cmsg_boundary_fast_path`, sends 60 kB through the socket state, a size in
   the bug's region for no buffer the state allows, so neither fails without the check.

## The sync to 1.4.0 (2026-10-31)

| upstream | before | now |
| --- | --- | --- |
| quinn#2727, `SO_SNDBUF` clear of the permanent `EWOULDBLOCK` (`9c42264c`, `49477cf0`) | patch 1 | 1.4.0's, the same code; dropped, its test kept (patch 3) |
| `EIO` from `sendmsg` in the `cmsg` fallback (`68e27fa8`); `cmsg` payloads read unaligned (`215f49ec`) | patches 4, 5 | 1.4.0's; dropped |
| `UdpSocketState::send` deprecated (`31032ff1`) | not taken | in; patch 1 keeps it whole |
| `is_msg_size_err` (`c3cac8af`) | — | in; noq's endpoint driver uses it on a receive |

quinn#2748 is still open and noq has no partial sends, so patches 1 and 2 stay. noq#778 (control
buffers sized from their payloads) and #746 (failing GSO batches split and resent) are open.

The fast path is compiled in only on macOS, by `onicode-net`'s target-specific `noq` feature,
and is on unless `ONICODE_BATCHED_UDP=0`. iOS never builds it: the calls are private and reached by `dlsym`.

Wired in with `[patch.crates-io]` in the root `Cargo.toml`, whose workspace excludes `vendor/`. The files are the crate as
published (its normalised `Cargo.toml`, `Cargo.lock`, `build.rs`, `src`, `tests`, `benches`,
licences) plus the patches, `[workspace]` at the end of `Cargo.toml`, and `rustfmt.toml`, noq's own
options (its repository's). `cargo fmt` here formats it by those options rather than the workspace's, so
`diff -ru` against the published crate shows only the patches. To move to a newer noq: take the new
published crate and re-apply what upstream has not taken. Drop patch 1 once noq has #2748 in any
form; patch 2 goes with it.

Tests: `cargo test --manifest-path vendor/noq-udp/Cargo.toml` runs the suite, and on a Mac again with `fast-apple-datapath`,
the build where patch 2, patch 1's partial `sendmsg_x` and the fast path's tests compile
(`noq.yml` runs it on every change here).
