//! Terminals on other devices, for any viewer: the desktop engine and the phone run the same
//! code, over the peer link to the terminal's host.
//!
//! - [`link`]: one terminal's stream (`StreamKind::Terminal`, named by its id), both ends, and the
//!   datagram copies of keys and echoes;
//! - [`remote`]: the viewer's hub, which numbers input, reopens a stream its link dropped and
//!   resumes after the last event it delivered: no gap, no repeat, no lost keystroke.

pub mod link;
pub mod remote;
