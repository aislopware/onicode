# noq-proto, vendored with twenty-three patches

The published `noq-proto` 1.4.0 (crates.io, upstream tag `noq-proto-v1.4.0`, commit
`34f75c49f` of <https://github.com/n0-computer/noq>) with these commits on top. Patches 1 to 20
keep the numbers they had on 1.3.0; 21 was 33 there, 22 undoes one of 1.4.0's changes, and
1.3.0's 21 to 32 are 1.4.0's own ("The sync to 1.4.0", below). Patch 23 is Axolotty's, on top
of Slopty's copy as it stood on 2026-11-03.

onicode took this copy from axolotty's `axquic` on 2026-11-06, unchanged. The patches were
written and measured in slopty and axolotty, so the measurements, `docs/` pages and test paths
named below are theirs; onicode's transport (`crates/net`, `onicode-net`) runs the same tunings,
and its seeded simulations (`crates/net/tests/sim.rs`) re-run axquic's.

1. `feat(proto): Add TransportConfig::stream_priority_before_datagrams`

   With `Some(t)`, a packet takes the STREAM frames of streams at priority `t` or above before
   its DATAGRAM frames, then the rest; `None` (the default) keeps noq's order, datagrams first.
   onicode sets it so control, RPC and terminal streams go ahead of datagrams, and tunnels and file
   transfers stay behind it (docs/decisions/transport.md, docs/MEASUREMENTS.md "an echo ahead
   of the datagrams").

2. `fix(proto): Clear BBR3's ACK-aggregation count on a restart from idle`

   `handle_restart_from_idle` moved `extra_acked_interval_start` to now and kept
   `extra_acked_delivered`, so after every idle gap each byte delivered counted as
   aggregation, and `extra_acked` and the window grew each other without bound. It now zeroes
   the count with the start, as Linux `tcp_bbr.c` does on `CA_EVENT_TX_START` and as the
   draft's own `OnInit` pairs them. The draft's `HandleRestartFromIdle` (draft-ietf-ccwg-bbr-06
   and the editor's copy) resets only the start. Test:
   `restart_from_idle_starts_an_empty_ack_aggregation_interval`.

3. `fix(proto): Mark the connection app-limited in BBR3's ProbeRTT`

   The draft's `HandleProbeRTT` begins with `MarkConnectionAppLimited()`, so the low
   delivery rates of a flow held to half a BDP do not read as the path's. noq left it out. Test:
   `probe_rtt_marks_its_samples_app_limited`.

4. `fix(proto): Let the pacer hold the controller's send quantum`

   A send asks the pacer for a whole MTU of credit, since the packet is not built yet, and at a
   pacing rate below 125 kB/s the bucket held one MTU. So every packet waited for the bucket to
   fill completely, and a small packet right after another (an ACK, a datagram) waited for the
   first one's bytes to be earned back. BBR3 on a lossy, app-limited connection paces at tens
   of kB/s, and that wait was 15 to 30 ms. The bucket is now never smaller than the
   controller's `send_quantum` (BBR's `C.send_quantum`, at least `2 * SMSS`,
   draft-ietf-ccwg-bbr-06 section 5.6.3); above the lowest rates the rate's own budget is
   larger and nothing changes. Test: `a_low_rate_lets_the_send_quantum_out_together`
   (docs/MEASUREMENTS.md, "datagram copies of a keystroke and its echo").

5. `fix(proto): Pad an Initial close to the header protection sample`

   `Endpoint::initial_close` (a refused or failed first packet) wrote the CONNECTION_CLOSE and
   the tag and nothing more, counting on a 16-byte AEAD tag to cover the header protection
   sample 4 bytes past the packet number. onicode's null crypto (from slopty) has no tag and claims a 16-byte
   sample, so every packet is long enough to draw a stateless reset (docs/decisions/transport.md,
   "The stateless reset a restarted host sends now arrives"). The close fell short of it, and
   the client dropped it as too short to decode. It now pads with PADDING frames up to the
   sample, which a real AEAD's tag already covers, so nothing changes for TLS. Test:
   `an_initial_close_holds_the_header_sample_when_the_tag_is_shorter`.

6. `fix(proto): Take BBR3's first round trip from the acknowledged packet`

   BBR3's first rate sample took its RTT from the `RttEstimator` it is handed, but the
   connection calls `on_ack` before it feeds that ACK's sample to the estimator, so on the first
   ACK the estimator still holds the configured initial RTT. That value became `BBR.min_rtt`.
   Upstream's 333 ms default is above any real path and the next sample replaces it, but an
   initial RTT below the path's stands for the ten seconds of `MinRTTFilterLen`, since no later
   sample is lower. Slopty's 5 ms against a 60 ms path sized the window to a twelfth of the
   bandwidth-delay product, and one bulk stream ran at 10 Mbit/s. Every later sample already
   used `now - P.send_time`, the draft's `RS.rtt` (draft-ietf-ccwg-bbr-06 section 4.1.2.3), and
   the first now does too. Test: `the_first_ack_measures_min_rtt_from_its_own_packet`
   (docs/MEASUREMENTS.md, "one bulk stream over a long round trip").

7. `feat(proto): Expose BBR3's min_rtt`

   `Bbr3::min_rtt` returns `BBR.min_rtt`. The congestion snapshot (`onicode_net::congestion`) reports it beside the
   path's own measured minimum, so the gate can check the model against the path whatever the
   machine's load. The rate a bulk stream reaches depends on free cores, and a rate floor
   failed three gates with another job on every performance core (docs/decisions/testing.md).
   This crate's own tests do not run in the workspace's gate, so patch 6's unit test cannot guard it
   there.

8. `feat(proto): Add TransportConfig::stream_priority_unpaced`

   With `Some(t)`, a datagram is started without the pacing check while a stream at priority
   `t` or above has data pending. The congestion window still applies, and the pacer is charged
   for the packet as usual. The pacer's timer runs on the runtime's clock, and tokio's is a
   millisecond wheel that macOS coalesces on top, so a packet the rate earns in half a
   millisecond waited one or two. onicode sets it to the echo's priority, so an echo behind paced
   video leaves at once: p90 1.4 ms fell to 0.25 ms at 20 Mbit/s (docs/MEASUREMENTS.md, "an
   echo behind paced video"). Tests: `stream_priority_unpaced_skips_the_pacer`,
   `stream_priority_unpaced_below_the_stream_waits`,
   `stream_priority_unpaced_off_waits_for_the_pacer`. Slopty's gate held it through
   `an_echo_does_not_wait_for_the_pacer` (`crates/slopty-net/tests/pacer_wait.rs`).

9. `feat(proto): Add Bbr3Config::probe_rng_seed`

   BBR3 draws where each bandwidth probe starts from a generator it seeds from the OS, and the
   config's seed field had no setter outside the crate's tests. The simulated network
   (`onicode_net_sim`, from slopty's `slopty_shape::sim`) seeds everything else a run draws, and with the probes left random a
   flooded link's echo times differed between two runs of one seed. `None` stays the default.
   onicode's gate holds it through `a_seed_repeats_its_run` (`crates/net/tests/sim.rs`).

10. `fix(proto): Keep a validated path to fall back to when a migration's check fails`

   `Connection::migrate` keeps the path it leaves so that a failed validation of the new one can go
   back to it. Its comment says it keeps that path only if it was validated, but the test was
   inverted (`!prev_path_data.validated`), so a migration away from a working path kept nothing.
   When the one PATH_CHALLENGE to the new address was lost, the retry waited on the
   anti-amplification budget the padded challenge had spent, `PathValidationFailed` fired first and
   cleared the challenge, and with nothing to go back to the path stayed unvalidated for good. noq
   sends no data on an unvalidated path, not even ACKs, while PINGs kept the connection from timing
   out: after a NAT rebinding the worker never sent the client another byte. With the test the right
   way round, the failed check goes back to the old path, and the client's next packet from its new
   address starts the migration again. noq 1.4.0 and `main` still have the inverted test (checked
   2026-10-31, `main` at `70fdbfa2`). Slopty's gate held it through
   `a_nat_rebinding_moves_the_connection_without_a_reconnect` (`crates/slopty-net/tests/sim.rs`,
   seed 10 loses the first challenge).

11. `fix(proto): Pace BBR3's Startup from the first round trip`

   The constructor has no round trip, so `BBRInitPacingRate` uses 1 ms and paces at
   `2.77 * InitialCwnd / 1 ms`, and Startup only ever raises the rate. A flow that stays
   application-limited, as video does, never leaves Startup and kept that placeholder, which
   on any path longer than a millisecond is no pacing at all (noq#800). The first ACK now
   re-derives the rate from its own packet's round trip, as Linux BBR's
   `bbr_init_pacing_rate_from_rtt` does on `has_seen_rtt`. This finishes noq#802 and
   quinn#2481, which take the RTT from the estimator, and the estimator does not yet hold the
   first sample when the controller sees the ACK (patch 6). Test:
   `the_first_round_trip_sets_the_startup_pacing_rate`.

12. `fix(proto): Cap BBR3's ACK-aggregation allowance at 100 ms of bandwidth`

   `BBRUpdateMaxInflight` adds `extra_acked` to the window with no bound. Linux caps it at
   `bbr_extra_acked_max_us`, 100 ms at `BBR.bw`, and so does this. Patch 2 removed the one runaway
   seen so far; the cap keeps any other (a clumping path, a count that outlives its interval) from
   growing the window with it. Test: `ack_aggregation_adds_at_most_100_ms_of_bandwidth`.

13. `fix(proto): Refresh BBR3's ProbeRTT filter from a flow's own quiet round trips`

   Video goes idle between frames, and the first packet of each frame leaves with no more in
   flight than ProbeRTT would allow (`BBRProbeRTTCwnd`, half a BDP). That packet's round trip
   is the measurement ProbeRTT takes by holding the window down for 200 ms, so when the filter
   has expired it refreshes from such a sample and the flow does not dip. A flow that was
   quiet within the last ProbeRTT's length waits for its next quiet sample rather than
   refreshing from a loaded one. A flow never that quiet still enters ProbeRTT as the draft
   says. This goes beyond draft-ietf-ccwg-bbr-06, whose only skip is on a restart from idle.
   Tests: `a_flow_quiet_between_frames_refreshes_min_rtt_without_probe_rtt`,
   `a_flow_never_quiet_still_probes_rtt`; `probe_bw_skips_probe_rtt_on_restart_from_idle` now
   checks that the resume packet's own round trip refreshed the filter. In `VideoSim`, ProbeRTT
   entries fell from 9 to 0 and P-frame p99 from 96 to 48 ms.

14. `feat(proto): Expose BBR3's state`

   `Bbr3::state_name` names the model's state as the draft does (`Startup`, `ProbeBW_UP`,
   `ProbeRTT`, ...). The congestion snapshot (`onicode_net::congestion`) carries it, and slopty's, simulated-network test
   asserts that video never meets ProbeRTT.

15. `feat(proto): Bundle a pending ACK into a packet that leaves anyway`

   Port of quinn-rs/quinn#2747 (merged into quinn 2026-09-02; noq#853, "wip ack bundling", is noq's
   own start on it, a draft). An ACK the delayed-ACK timer holds went out in a packet of its own
   when the timer fired, even when an ack-eliciting packet had left on the same path in the
   meantime. `populate_packet` now adds it, when it fits, to any ack-eliciting packet on its own
   path that carries no ACK yet (`PendingAcks::can_bundle`). An echo now carries the ACK of the key
   it answers: in Slopty's simulated network the worker sends 1.06 packets per key instead of 2.02.
   The ACK frame is built by a new `ack_frame`, shared with `populate_acks`, and sized without
   encoding by `AckEncoder::size` and `PathAckEncoder::size` (3.5 to 10 ns for one to eight ranges,
   where encoding into a `Vec` to measure it took 27 to 99 ns; `ack_size_is_its_encoded_length`
   holds the two equal). `SentFrames::is_ack_only` now also counts `path_retransmits`, since a
   packet holding OBSERVED_ADDRESS and a bundled ACK is not ACK-only, and the debug assertion that
   relies on it fired in the address-discovery tests. Quinn's merged version holds bundling back for
   a round trip and the peer's `max_ack_delay` after an ACK unless an ack-eliciting packet arrives;
   `can_bundle` bundles only while one has arrived since the last ACK, so it never bundles more
   often. Test: `ack_bundled_with_datagrams`. `path_open_challenge_lost` advanced wakeups a fixed
   number of times to reach the third challenge; bundling moves a probe timeout in between, so it
   now advances until the challenge count rises.

16. `fix(proto): Keep a queued datagram exactly at the new limit`

   Part of quinn-rs/quinn#2839 (merged into quinn 2026-09-10, not in noq). When black-hole detection
   lowers the MTU, queued datagrams that no longer fit are dropped, but the test was `len < max`, so
   one exactly at the new limit went too, although `send` accepts that size. onicode's minimum MTU
   sits below its initial one, so the drop can happen. The rest of #2839 prunes the queue after a
   path reset or migration; here those only ever raise the MTU back to the initial 1232, so it is
   not taken. 1.4.0 moved the queue into `DatagramBuffer` (#823) and kept the `<` in
   `DatagramState::drop_oversized`. Test: `drop_oversized_keeps_datagrams_at_limit`.

17. `fix(proto): Take only a short-header packet for a stateless reset`

   `CryptoState::unprotect_header` compared the last 16 bytes of every packet with the peer's
   reset token, a long-header packet coalesced ahead of others included, and a match ended the
   connection even when the packet decrypted. RFC 9000 compares the datagram's trailing bytes
   (section 10.3.1), and a stateless reset takes the form of a short-header packet (section
   10.3), which always runs to the end of its datagram. Behind an AEAD a packet ends in its tag,
   so the check never went wrong. onicode's null crypto has no tag. A server's Initial ends in its
   transport parameters, whose order noq shuffles, and one time in about twelve the reset token
   comes last. Sent coalesced, the Initial takes no padding, so a resent copy that the client
   read before it had answered the first ended in the token it had just learnt. The dial then
   failed with "reset by peer" (docs/decisions/transport.md, "A resent Initial is not a stateless
   reset"). Only a short-header packet is compared now. Tests:
   `a_long_header_packet_ending_in_the_token_is_no_stateless_reset` and
   `a_short_header_packet_ending_in_the_token_is_a_stateless_reset`.

18. `fix(proto): coalesce contiguous chunks during defragment` (noq#866, `3ff2822f`) and
    `fix(proto): keep unread data when compacting an unordered stream` (noq#867, `b70f1f2e`)

   noq `main`'s two assembler fixes after 1.4.0, cherry-picked. RUSTSEC-2026-0185
   (CVE-2026-25800): a peer that sends small STREAM or CRYPTO frames with gaps between them made
   the `Assembler` keep every chunk while the reader waited for the missing prefix. 1.4.0 has
   quinn's bound (quinn-rs/quinn#2694, #2789: `insert` refuses with `TooManyChunks` past 1024
   chunks left after defragmenting, which closes the connection with `INTERNAL_ERROR`) on a
   sorted `VecDeque` (#2803), but not #2814: a packet-sized chunk, once defragmented, is never
   merged again, so a reader about 2 048 packets behind on a stream with no gap at all is refused
   with "too many gaps in stream buffer". #866 ports #2814 onto the deque (contiguous chunks
   smaller than `buffered / MAX_CHUNKS`, at least 128 bytes, are merged while defragmenting).
   #867 fixes compaction in unordered mode, which discarded data below `bytes_read`, there a
   count of delivered bytes rather than an offset (noq#865). With both this file is `main`'s as
   of `70fdbfa2`. Before 1.4.0 this patch was quinn 0.11.18's assembler on noq's old heap
   (noq#828, closed for #866). onicode reads every stream in order, so #867 does not reach it;
   #866 does: a stream its reader falls far behind on (a scrollback replayed into a busy
   client) is not refused. Tests: upstream's, in `assembler.rs`
   (`ordered_lossless_stream_is_not_rejected`, `unordered_compaction_keeps_unread_data` and
   quinn's).

19. `fix(proto): Advance BBR3's max_bw filter once per probe`

   `adapt_long_term_model` advanced the `max_bw` filter on every round start while `ack_phase`
   was `ProbeStopping`, and nothing moved it on, so from each ProbeBW_DOWN until the next probe
   the filter's two-cycle window was two round trips. `max_bw` then followed the latest delivery
   rate down, and with random loss cutting `bw_shortterm` every round the two ratcheted a bulk
   stream from 450 to 10 Mbit/s within seconds. noq follows draft-05, which left the step out.
   The draft's editor's copy and Linux `tcp_bbr.c` (`bbr_adapt_upper_bounds`) end the probe's
   feedback once: `ack_phase` goes to `ACKS_INIT` and `bw_probe_samples` is cleared, so a loss
   after the probe no longer counts as the probe's. Test:
   `the_max_bw_filter_advances_once_per_probe` (docs/MEASUREMENTS.md, "a connection's second
   bulk stream").

20. `fix(proto): Hold BBR3's ProbeBW_UP while inflight_longterm limits it, to a packet`

   ProbeBW_UP goes on while the flow is cwnd-limited at `inflight_longterm`. That ack's
   `probe_inflight_long_term_upward` has already grown it by bytes `cwnd` takes up only after,
   so noq's byte comparison was short on nearly every ack and the probe ended on the plateau
   `inflight_longterm` itself held. Linux compares them in packets, where `inflight_hi` steps
   once every `bw_probe_up_cnt` packets. `maybe_go_down` now counts `cwnd` within one SMSS of it
   as at it. Patch 19 exposed it: A.15 (`probe_up_rediscovers_full_bw_after_10x_increase`) had
   passed only on the filter's decayed `max_bw`. Test:
   `probe_up_goes_on_while_inflight_longterm_holds_it`.

21. `fix(proto): Take a stateless reset only from a datagram its own connection IDs did not route`

   After patch 17, `unprotect_header` still compared the last 16 bytes of every short-header
   packet with the peer's reset token, a packet that decrypted included. Behind an AEAD a packet
   ends in its tag. Under the null crypto it ends in its last frame, and a
   `NEW_CONNECTION_ID` ends in the very token it hands over. A server moves to the first of the
   client's IDs it reads, at once ("enable clientside stateless resets"), and takes that ID's
   token as the peer's. The client issues its four IDs together; a batched send (Apple's
   `sendmsg_x`, Linux GSO) cuts each datagram after the first to the first one's size, so a
   batch that resends them after a loss can carry one alone, the last frame of its packet. When
   such a packet carried the ID the server had moved to, it ended in that ID's token. Slopty's
   worker took it for a stateless reset, drained without a CONNECTION_CLOSE and answered the
   client with resets of its own, and the client failed with "reset by peer"
   (docs/decisions/transport.md, "A lone `NEW_CONNECTION_ID` is no stateless reset").
   `ConnectionIndex::get` now also says whether one of the connection's own non-empty IDs found
   it, the endpoint hands that over as `DatagramConnectionEvent::routed_by_cid`, and
   `handle_decode` compares no packet of such a datagram. A stateless reset's connection ID is
   unpredictable bits (RFC 9000 §10.3), so a genuine one is found by its token, the index's last
   step, and is still taken; §10.3.1 lets an endpoint skip the check for a datagram whose packet
   it processes. The datagram that creates a connection is never compared. Upstream compares
   every packet, which behind a tag never goes wrong. Tests: Slopty's
   `a_lone_new_connection_id_is_no_stateless_reset` (`crates/slopty-net/tests/sim.rs`, whose
   seed 172 failed before it), and for genuine resets noq's `client_stateless_reset` and
   `server_stateless_reset` with Slopty's
   `a_restarted_worker_resets_the_old_connection_within_a_keep_alive` and
   `a_ping_draws_a_restarted_worker_s_reset_at_once` (`crates/slopty-net/tests/loopback.rs`).

22. `fix(proto): Keep the last packet of the congestion window free`

   Reverts `9612584d` (noq#823, in 1.4.0), which lets a packet fill the congestion window to the
   byte (`>=` to `>` in `path_congestion_check`). RFC 9002 allows that packet, and upstream's
   `full_initial_window` asks for it. Of a window that is whole packets it sends one more into the
   bottleneck's queue, and BBR3's window often is: it grows by the packets acknowledged, and its
   floor is four. Beside a flood on Slopty's simulated network (2 Mbit/s, 100 ms of queue, 5 % lost
   each way), one congestion check in twenty met a window the next packet filled exactly: BBR3's own
   in nine of ten, its four-packet floor in the rest, Slopty's bound (`slopty_net::congestion`)
   never. With the fill, 657 of 1 200 seeds' echo p99 got slower and 499 faster, by 3 ms at the
   median and 1.8 ms on average (one packet at that rate is 4.9 ms), and 12 % more echoes took over
   150 ms; the median echo did not move. The slow echoes are the ones a loss sends round the queue
   again, and the extra packet sits in it each time. One more packet a window is worth nothing to
   Slopty; a keystroke's echo is. Without the fill every simulated scenario reads as on 1.3.0 to the
   millisecond (docs/MEASUREMENTS.md, "noq 1.4.0 on the simulated network"). Slopty's 16-packet
   floor so holds 15 full packets in flight, and BBR3's four-packet floor three. Test:
   `a_packet_never_fills_the_window_to_the_byte` (upstream's `full_initial_window`, the other way
   round).

23. `feat(proto): A stream at the unpaced priority writes past the send window` (Axolotty)

   `TransportConfig::stream_priority_unpaced` also exempts its streams from `send_window`: they are
   held by the peer's flow control alone. Slopty's sessions send frames two at a time, so its
   send buffer never fills. Axolotty sends a pane's raw output, and twelve panes streaming a
   build log filled noq's 10 MB send window. An echo's write then waited until acknowledgements
   freed room, and the flood's twelve writers took that room as fast as it came. On a simulated
   5 MB/s Wi-Fi link (`Rust/axquic/tests/sim.rs`), the echo beside that flood had a p99 of
   100–450 ms in half of 30 seeds, with nothing lost and nothing overflowed. Exempt, every one
   of 100 seeds kept the echo within 20 ms of the idle link's. Tests:
   `stream_priority_unpaced_writes_past_a_full_send_window`, and the two the other way round.

A probe-up exit for an app-limited round (leaving `ProbeBW_UP` when a round ends
app-limited) was tried beside patches 11 to 13 and not taken. The draft and Linux keep such a
flow in `ProbeBW_UP`, and with patches 11 to 13 the simulated keyframe p99 already matches
Cubic's, so it had nothing left to win (docs/decisions/transport.md, "BBR3 on app-limited
video").

Commits 2, 3 and 13 share `VideoSim` in the `bbr3` tests: a screen encoder's frames through one
bottleneck, paced by noq's token bucket. `video_through_one_bottleneck` (ignored) prints what
BBR3 does with that traffic (docs/MEASUREMENTS.md, "noq's BBR3 against the draft").

## The sync to 1.4.0 (2026-10-31)

1.4.0 (2026-10-20) is 109 commits past 1.3.0. What those in `noq-proto` mean here:

| upstream | before | now |
| --- | --- | --- |
| #792 no coalescing into a too small tail (`f916a5c5`); #822 a close matches an unspecified local IP (`34ec9691`) | patches 21, 22, taken ahead of the release | 1.4.0's, the same code; dropped |
| #823's ports of quinn's fixes: CUBIC's saturating increment, the CID refactors and the bound on retired CIDs, `DatagramBuffer` and its memory limits both ways, the black-hole detector's equal-size evidence, a close while congestion-blocked, §12.4's packet-type rules, a typo | patches 23–30 and 32, cherry-picked 2026-10-17 | 1.4.0's, the same code; dropped |
| #823's assembler: quinn#2694 and #2789 (`61302fd3`, `b08b5e1a`) on a sorted `VecDeque` (quinn#2803, `4523eaa6`) | patch 18 (all three quinn fixes on the old heap) and 31 | 1.4.0's, with `main`'s #866 and #867 on it (patch 18) |
| #823's exact window fill (`9612584d`: a packet may fill the congestion window to the byte, `>=` to `>`) | left out on a measurement | 1.4.0's, undone by patch 22 on a measurement of 1 200 seeds. 2026-10-17's reading (the 99th percentile of 200 seeds' echo p99, 235 to 293 ms) was two seeds; over 1 200 it reads 251 to 256 ms, and the cost is the smaller, steady one patch 22 gives |
| #823's BBR bandwidth estimate (`dcb8949c`) | not taken | in: `PathStats::bandwidth_estimate`, BBR3's `max_bw` in bit/s. Slopty's congestion snapshot reads BBR3's own state (patches 7, 14), as onicode's does, and nothing reads the field |
| #842 unread bytes credited once for a stream both stopped and reset (`13b93f70`) | — | in: the connection's flow-control window grew twice for such a stream's unread bytes, so a peer could send past it. onicode ends a stream both ways (an RPC call cancelled), so it reaches onicode |
| #740 sent packet and byte counts, #827 qlog's time-threshold losses, #838 and #845 (`core` and `alloc` imports, hash map aliases), #797 and #798 (tests) | not taken | in; nothing onicode does changes. The patches' tests import `Vec` and `println` as the `no_std` crate now asks |

The `noq` crate, which comes from crates.io at the same release, brings what reaches onicode
outside `noq-proto`:

- The endpoint driver copies each received batch into one reused buffer and splits the
  datagrams off it (`941eef95`): one allocation a batch where there was one a datagram, on
  every datagram a peer receives.
- #824 wakes `wait_idle` and `wait_all_draining` when the driver is dropped, so the server's
  `wait_idle` at shutdown no longer sits out its one-second timeout when the driver ended first.
- The driver ignores a receive's `EMSGSIZE`, an ICMP Packet Too Big (`c3cac8af`), and noq's own
  tokio socket now hands a failed receive up, where it looped on it (`099284e7`). onicode's socket
  (`onicode_net::udp`, from slopty) is its own and still retries a failed receive, as noq 1.3.0's did: handed
  up, any error but `ConnectionReset` and `EMSGSIZE` ends the endpoint and every connection on
  it, and the errors an unconnected UDP socket's receive returns here are one-shot (noq-udp asks
  for no `IP_RECVERR`), so the retry cannot spin.
- noq's own sender logs a refused send and goes on, as onicode's (`Shared::refused`) already does.

Not taken from `main` past 1.4.0 (read through `70fdbfa2`, 2026-10-31): #835 deprecates
`send_fairness` and removes unfair scheduling, which onicode never asks for; #836 lets a send
stream prefer a path, and onicode runs one; #868 bumps tokio in noq's own lock only. Open pull requests that would retire patches: #853 (ACK bundling, patch 15) and
#802 (Startup's pacing rate, half of patch 11).

`noq-proto` and `noq-udp` (`vendor/noq-udp/SLOPTY.md`) are patched (`[patch.crates-io]` in the
root `Cargo.toml`, whose workspace excludes `vendor/`); `noq` comes from crates.io at the same release. The files are the crate as
published (its normalised `Cargo.toml`, `Cargo.lock`, `src`, `benches`, `proptest-regressions`,
licences) plus the patches, `[workspace]` at the end of `Cargo.toml`, and `rustfmt.toml`, noq's own
options (its repository's), so that rustfmt run here formats as upstream does and not by the
workspace's settings. To move to a newer noq: take the new published crate and re-apply each patch (the
diff is `diff -ru` of this directory against the published files), or drop this directory once
upstream has them all. The patches are formatted by those options, so the diff holds no
reformatting. `cargo test --manifest-path vendor/noq-proto/Cargo.toml` runs this crate's own suite.
