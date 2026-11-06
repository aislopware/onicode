//! Previews: project-scoped HTTP discovery, stable local routing through one loopback proxy, and
//! the services of the user's other devices reached over the peer links, one tunnel stream per
//! connection ([`tunnel`]).
pub mod catalog;
pub mod discovery;
pub mod login;
pub mod proxy;
pub mod tunnel;

pub mod service;
pub use service::{PreviewService, Scope};
