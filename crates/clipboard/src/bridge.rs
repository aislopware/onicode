//! A promise is answered synchronously, from whatever thread asks (AppKit's main thread for the
//! general pasteboard), while its bytes come from the other Mac asynchronously. The fetch runs on
//! the runtime under its own deadline; the asking thread waits for the answer, running the main
//! run loop between looks when it is the main thread, so the app's windows (or desktopd's made
//! displays) keep going and nothing the fetch needs can be stuck behind the wait.

use std::future::Future;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;

#[cfg(target_os = "macos")]
use crate::board::serve_main_run_loop;

/// How long the waiting thread sleeps between looks at the main run loop.
const SLICE: Duration = Duration::from_millis(5);
/// Past the deadline, how long a waiter gives a runtime that stopped answering: the fetch's
/// own deadline is on the runtime's clock, and a runtime that is shutting down never reads it.
const BACKSTOP: Duration = Duration::from_secs(2);

/// Runs `work` on `runtime` for at most `deadline` and waits for its answer here.
///
/// Never call it on a thread of `runtime` itself: a current-thread runtime would wait on
/// itself until the backstop. Readers inside the runtime read a board from `spawn_blocking`.
pub(crate) fn wait<T, F>(runtime: &Handle, deadline: Duration, work: F) -> Option<T>
where
    T: Send + 'static,
    F: Future<Output = Option<T>> + Send + 'static,
{
    let (tx, rx) = sync_channel(1);
    let _task = runtime.spawn(async move {
        let got = tokio::time::timeout(deadline, work).await.ok().flatten();
        let _sent = tx.send(got);
    });
    let backstop = Instant::now().checked_add(deadline.saturating_add(BACKSTOP));
    loop {
        match rx.recv_timeout(SLICE) {
            Ok(got) => return got,
            Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) => {
                if backstop.is_some_and(|end| Instant::now() >= end) {
                    return None;
                }
                // AppKit's main run loop is the one a waiting promise must keep serving.
                #[cfg(target_os = "macos")]
                let _served = serve_main_run_loop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer comes back to the waiting thread; a fetch past its deadline is `None` by then,
    /// on the runtime's clock: a paused one, which jumps to the deadline once nothing else is due.
    #[test]
    fn a_wait_ends_with_the_answer_or_the_deadline() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let handle = runtime.handle().clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let driver = std::thread::spawn(move || {
            let _stopped = runtime.block_on(stopped);
        });
        assert_eq!(
            wait(&handle, Duration::from_secs(3), async { Some(7) }),
            Some(7)
        );
        let never = wait(
            &handle,
            Duration::from_secs(60),
            std::future::pending::<Option<u8>>(),
        );
        assert_eq!(never, None, "the deadline, in virtual time");
        stop.send(()).unwrap();
        driver.join().unwrap();
    }
}
