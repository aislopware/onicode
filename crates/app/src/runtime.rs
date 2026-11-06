//! Turns of `remote://` sessions, run through the SSH-tunnelled Node host. Local turns run in
//! `onicode_engine::turn`; this goes when the engine reaches other machines over peers
//! (`docs/onicode/decisions/engine.md`, migration step 10).
pub(crate) mod remote;
