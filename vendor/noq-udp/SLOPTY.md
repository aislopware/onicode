# noq-udp, vendored with four patches

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
   tripping a `debug_assert`. (The fallback when `sendmsg_x` could not be resolved, which sent
   one datagram, went with patch 4.) onicode's socket (`onicode_net::udp`, from slopty) keeps the part
   not yet sent across a wait for room, which is quinn's connection-side half. 1.4.0
   deprecates `send` (#823, `31032ff1`); it goes on after a partial send as `try_send` does.
   Tests: `apple_fast_datapath_reports_a_partial_send`, `a_whole_send_reports_every_datagram`.

2. `feat(udp): Enable the Apple fast path only where the OS has it`

   `UdpSocketState::try_enable_apple_fast_path` resolved `sendmsg_x` and `recvmsg_x` first and
   enabled the path only when both existed, which was the one condition the `unsafe`
   `set_apple_fast_path` left to its caller. Superseded by patch 4: the calls are linked
   directly, so `set_apple_fast_path` is safe and `try_enable_apple_fast_path` is gone. noq's own
   runtime never enables the path; `onicode-net`, which forbids `unsafe`, calls
   `set_apple_fast_path` on macOS.

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

4. Apple-only (macOS on Apple silicon, iOS, 27.2): non-Apple platform code removed, 2026-11-07

   onicode runs on macOS and iOS 27.2 alone (`docs/onicode/decisions/platform.md`), so the crate
   keeps the Apple path and nothing else. Deleted: `src/windows.rs`, `src/cmsg/windows.rs`,
   `src/linux.rs` (GSO/GRO, the kernel-version probe), `src/posix_minimal.rs` (ESP-IDF, WASI),
   `build.rs` and its `cfg_aliases` build dependency (the `apple`, `bsd`, `solarish`,
   `apple_fast`, `apple_slow`, `wasm_browser`, `posix_minimal` aliases: code now tests
   `feature = "fast-apple-datapath"` alone), the `windows-sys` dependency and dev-dependency, and
   the wasm gate on `socket2`, now a plain dependency. In `src/unix.rs`, the `recvmmsg` receive,
   the Linux `sendmsg` with its EINVAL/EIO fallback (`sendmsg_einval`, for Linux before 3.13),
   `IP_PKTINFO`, `IP_MTU_DISCOVER`, `UDP_GRO`, `SCM_TIMESTAMPNS`, the FreeBSD, OpenBSD, NetBSD,
   Dragonfly, Solaris, illumos and Redox branches, and the no-op `gso` module went;
   `RecvMeta::timestamp` (Linux only, and off upstream since noq#774) went with them, and
   `RecvMeta::stride` is the datagram's length. The OS-age checks went too:
   `IP_DONTFRAG` and `IPV6_DONTFRAG` are set as `IPV6_RECVPKTINFO` is (an error fails
   `UdpSocketState::new`), so `may_fragment` is `false` and `gro_segments` 1; `sendmsg_x` and
   `recvmsg_x` are declared `extern "C"` and linked from `libsystem_kernel` (both SDKs' `.tbd`
   export them) instead of resolved by `dlsym`, which removes `available`, `resolve_symbol`,
   `resolve_apple_fast_fn`, `disable_apple_fast_path`, the per-call fallbacks to `sendmsg` and
   `recvmsg`, and `Transmit::first`; `max_gso_segments` reads the fast-path flag. Kept on
   purpose: the zeroed control buffers for `recvmsg_x` and the zero-length `cmsg` stop in
   `cmsg::Iter`, which guard against `msg_controllen` left as given whatever the OS release.
   `prepare_msg` and `prepare_msg_x` share `encode_control`. Tests that exercised only other
   platforms (`gso`, the Windows `recv_survives_icmp_unreachable_from_prior_send`, the Wine and
   Android branches, the timestamp checks) went; the Apple ones run unconditionally or under the
   feature. The bench compares the plain and batched paths instead of GSO/GRO/`recvmmsg`.
   Re-vendoring: take the published crate, delete the same files, and keep `unix.rs` to its
   Apple branches.

## The sync to 1.4.0 (2026-10-31)

| upstream | before | now |
| --- | --- | --- |
| quinn#2727, `SO_SNDBUF` clear of the permanent `EWOULDBLOCK` (`9c42264c`, `49477cf0`) | patch 1 | 1.4.0's, the same code; dropped, its test kept (patch 3) |
| `EIO` from `sendmsg` in the `cmsg` fallback (`68e27fa8`); `cmsg` payloads read unaligned (`215f49ec`) | patches 4, 5 | 1.4.0's; dropped |
| `UdpSocketState::send` deprecated (`31032ff1`) | not taken | in; patch 1 keeps it whole |
| `is_msg_size_err` (`c3cac8af`) | — | in; noq's endpoint driver uses it on a receive |

quinn#2748 is still open and noq has no partial sends, so patch 1 stays. noq#778 (control
buffers sized from their payloads) and #746 (failing GSO batches split and resent) are open.

The fast path is compiled in only on macOS, by `onicode-net`'s target-specific `noq` feature,
and is on unless `ONICODE_BATCHED_UDP=0`. iOS never builds it: the calls are private API.

Wired in with `[patch.crates-io]` in the root `Cargo.toml`, whose workspace excludes `vendor/`. The files are the crate as
published (its normalised `Cargo.toml`, `Cargo.lock`, `src`, `tests`, `benches`,
licences) plus the patches, `[workspace]` at the end of `Cargo.toml`, and `rustfmt.toml`, noq's own
options (its repository's). `cargo fmt` here formats it by those options rather than the workspace's, so
`diff -ru` against the published crate shows only the patches (patch 4 deletes files and branches outright). To move to a newer noq: take the new
published crate and re-apply what upstream has not taken. Drop patch 1 once noq has #2748 in any
form; patch 4 is re-applied to every new copy.

Tests: `cargo test --manifest-path vendor/noq-udp/Cargo.toml` runs the suite, and on a Mac again with `fast-apple-datapath`,
the build where patch 1's partial `sendmsg_x` and the fast path's tests compile
(`noq.yml` runs it on every change here).
