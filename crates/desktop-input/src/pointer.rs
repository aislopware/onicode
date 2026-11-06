//! Where a stream's input last put the host's pointer, for the stream's cursor samples.
//!
//! A display stream's events go through the HID tap and move the host's own pointer, so the
//! real pointer is the one to show ([`Pointer::Real`]). A sink whose input leaves the real pointer
//! alone reports where it put it instead ([`Pointer::Placed`]), or nowhere before the first event.

use tokio::sync::watch;

/// Where the host's pointer is, as far as a stream's input is concerned.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pointer {
    /// The input moves the host's own pointer: read the real one.
    Real,
    /// The input leaves the host's pointer alone: the last place it was put, in global display
    /// points, `None` before the first.
    Placed(Option<(f64, f64)>),
}

/// A [`Pointer`] the input thread writes and any other reads or waits on.
///
/// Starts as `Placed(None)`. A write that changes nothing wakes nobody, so a reader that waits
/// on [`Self::changes`] sleeps for as long as the input leaves the pointer where it is.
#[derive(Clone, Debug)]
pub struct PointerWatch(watch::Sender<Pointer>);

impl Default for PointerWatch {
    fn default() -> Self {
        Self(watch::Sender::new(Pointer::Placed(None)))
    }
}

impl PointerWatch {
    /// The pointer as last written.
    #[must_use]
    pub fn get(&self) -> Pointer {
        *self.0.borrow()
    }

    /// Wakes on every write that changed the pointer from here on: [`PointerChanges::changed`].
    #[must_use]
    pub fn changes(&self) -> PointerChanges {
        PointerChanges(self.0.subscribe())
    }

    /// The input moves the host's own pointer.
    pub fn follow_real(&self) {
        self.set(Pointer::Real);
    }

    /// The input put the pointer at `(x, y)` global display points. A point that is not finite
    /// was never posted anywhere and is ignored.
    pub fn place(&self, x: f64, y: f64) {
        if x.is_finite() && y.is_finite() {
            self.set(Pointer::Placed(Some((x, y))));
        }
    }

    fn set(&self, pointer: Pointer) {
        self.0.send_if_modified(|now| {
            let changed = *now != pointer;
            *now = pointer;
            changed
        });
    }
}

/// One reader's view of a [`PointerWatch`]'s changes.
#[derive(Debug)]
pub struct PointerChanges(watch::Receiver<Pointer>);

impl PointerChanges {
    /// Resolves once the pointer changed since this was made or last resolved. It never
    /// resolves while the watch it came from is held, which is what the caller holds to read
    /// the pointer.
    pub async fn changed(&mut self) {
        if self.0.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_placed_point_reads_back_and_nothing_else_looks_like_one() {
        let watch = PointerWatch::default();
        assert_eq!(watch.get(), Pointer::Placed(None));
        watch.place(-1512.5, 982.25);
        assert_eq!(watch.get(), Pointer::Placed(Some((-1512.5, 982.25))));
        watch.place(f64::NAN, 3.0);
        assert_eq!(
            watch.get(),
            Pointer::Placed(Some((-1512.5, 982.25))),
            "a NaN is not a place"
        );
        watch.follow_real();
        assert_eq!(watch.get(), Pointer::Real);
    }

    /// A reader waiting on the changes wakes once per move and not for a write that put the
    /// pointer where it already was, or one that was ignored.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn only_a_move_wakes_a_reader() {
        let watch = PointerWatch::default();
        let mut changes = watch.changes();
        let quiet = Duration::from_secs(60);
        watch.place(10.0, 20.0);
        tokio::time::timeout(quiet, changes.changed())
            .await
            .unwrap();
        watch.place(10.0, 20.0);
        watch.place(f64::NAN, 1.0);
        assert!(
            tokio::time::timeout(quiet, changes.changed())
                .await
                .is_err(),
            "the same place, or no place, is not a move"
        );
        watch.follow_real();
        tokio::time::timeout(quiet, changes.changed())
            .await
            .unwrap();
    }
}
