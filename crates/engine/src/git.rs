//! Git (`docs/onicode/decisions/engine.md`, migration step 5): checkout status watches over the
//! engine's file watch ([`CheckoutHub`], the protocol's `WatchGit`), and the Git calls behind the
//! Changes, review, history, branch and worktree views, served as the Git group by
//! [`GitService`]. Every Git command runs in one of two [`Lane`]s, so a person's call never
//! queues behind background work.
mod hub;
mod lanes;
mod service;
#[cfg(test)]
mod tests;

pub use hub::CheckoutHub;
pub use lanes::Lane;
pub use service::GitService;
