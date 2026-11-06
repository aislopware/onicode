//! `onicode-desktopd`: the Mac's remote desktop host, started by launchd as the app's login-item
//! agent.
//!
//! - no argument: serve. The main thread parks in the run loop the made displays live on, and the
//!   runtime runs beside it.
//! - `--probe-grants`: print the grants a new process reads and exit
//!   (`onicode_desktop_grants::answer_probe_if_asked`, before anything else).
//! - `--display-holder <socket> <list>`: be the display holder ([`onicode_desktopd::holder`]).
//! - `--synthetic`: serve drawn displays; never captures, never needs a grant.
//! - `doctor`: print the grants as the session tells them, without prompting.

#![deny(unsafe_code)]

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use onicode_desktop_link::ctl::{CtlReply, CtlRequest};
use onicode_desktop_link::{HostGrants, ctl as link_ctl};
use onicode_desktopd::platform::{Native, Platform};
use onicode_desktopd::session::{self, Host};
use onicode_desktopd::sized::{self, Displays, Factory};
use onicode_desktopd::{ctl, grants};
use parking_lot::Mutex;

/// How long desktopd waits before binding its endpoint again while the tailnet is down.
const BIND_AGAIN: Duration = Duration::from_secs(5);

/// The grants a synthetic run claims: it captures nothing, so it needs none.
fn all_granted() -> HostGrants {
    HostGrants {
        screen_recording: true,
        accessibility: true,
        post_event: true,
    }
}

fn main() -> ExitCode {
    #[cfg(target_os = "macos")]
    onicode_desktop_grants::answer_probe_if_asked();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(target_os = "macos")]
    if args.first().map(String::as_str) == Some(onicode_desktopd::holder::HOLDER_ARG) {
        return holder_main(&args);
    }
    if args.first().map(String::as_str) == Some("doctor") {
        return doctor();
    }
    let synthetic = args.iter().any(|a| a == "--synthetic");
    let data_dir = onicode_desktopd::data_dir();
    serve(data_dir, synthetic)
}

/// Print the grants without prompting.
fn doctor() -> ExitCode {
    let grants = grants::current();
    let line = serde_json::to_string(&grants).unwrap_or_default();
    let missing = grants.missing();
    let mut out = std::io::stdout().lock();
    let written = writeln!(out, "{line}").and_then(|()| {
        if missing.is_empty() {
            writeln!(out, "every grant is on")
        } else {
            writeln!(out, "missing: {missing:?}")
        }
    });
    if written.is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Serve, on a Mac: the made displays and the main queue on the main thread, the runtime on a
/// thread beside it, ending the process when it ends.
#[cfg(target_os = "macos")]
fn serve(data_dir: PathBuf, synthetic: bool) -> ExitCode {
    use onicode_desktopd::holder;

    let made = if synthetic {
        None
    } else {
        sized::on_main_queue(holder::Remote::unlinked())
    };
    let spawned = std::thread::Builder::new()
        .name("onicode-desktopd".to_owned())
        .spawn(move || {
            let ended = runtime().map(|runtime| {
                runtime.block_on(async move {
                    if let Some(made) = &made {
                        hold_displays(made, &data_dir);
                    }
                    if synthetic {
                        run::<onicode_desktopd::synthetic::Canvas, holder::Remote>(
                            data_dir,
                            None,
                            all_granted,
                        )
                        .await
                    } else {
                        run::<Native, holder::Remote>(data_dir, made, grants::current).await
                    }
                })
            });
            let code = match ended {
                Ok(Ok(())) => 0,
                Ok(Err(e)) | Err(e) => {
                    tracing::error!(error = %e, "desktopd ends");
                    1
                }
            };
            #[expect(
                clippy::exit,
                reason = "the main thread never returns; this ends the process"
            )]
            std::process::exit(code);
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "no thread for desktopd");
        return ExitCode::FAILURE;
    }
    sized::park_main()
}

/// Serve, off a Mac: nothing to capture, but the socket and the endpoint answer.
#[cfg(not(target_os = "macos"))]
fn serve(data_dir: PathBuf, synthetic: bool) -> ExitCode {
    let ended = runtime().map(|runtime| {
        runtime.block_on(async move {
            if synthetic {
                run::<onicode_desktopd::synthetic::Drawn, sized::Cg>(data_dir, None, all_granted)
                    .await
            } else {
                run::<Native, sized::Cg>(data_dir, None, grants::current).await
            }
        })
    });
    match ended {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(e)) | Err(e) => {
            tracing::error!(error = %e, "desktopd ends");
            ExitCode::FAILURE
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("onicode-desktopd-rt")
        .build()
        .map_err(|e| format!("no runtime: {e}"))
}

/// Have the made displays held by the display holder, its socket and list in the data
/// directory, and take those it holds from the desktopd before ([`onicode_desktopd::holder`]);
/// once more a little later, for those a holder that had to go is still making again.
#[cfg(target_os = "macos")]
fn hold_displays(made: &Displays<onicode_desktopd::holder::Remote>, data_dir: &std::path::Path) {
    use onicode_desktopd::holder;

    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(e) => {
            tracing::warn!(error = %e, "no display holder: every display streams a physical one");
            return;
        }
    };
    let dial = holder::spawning(
        program,
        data_dir.join("displays.sock"),
        data_dir.join("displays.json"),
    );
    made.with_factory(move |remote| remote.set_dial(dial));
    made.set_linger(sized::LINGER);
    made.adopt();
    let again = made.clone();
    tokio::spawn(async move {
        tokio::time::sleep(holder::ADOPT_AGAIN_AFTER).await;
        again.adopt();
    });
}

/// desktopd's life: the grants, the control socket, the endpoint once the tailnet is up, and
/// every client on it.
async fn run<P: Platform, F: Factory>(
    data_dir: PathBuf,
    made: Option<Displays<F>>,
    read_grants: fn() -> HostGrants,
) -> Result<(), String> {
    let grants = grants::follow(read_grants);
    let host = Host::<P, F>::new(onicode_desktopd::host_name(), grants, made);
    let addrs: Arc<Mutex<Vec<SocketAddr>>> = Arc::default();
    let socket = link_ctl::socket_path(&data_dir);
    let listener =
        ctl::listen(&socket).map_err(|e| format!("control socket {}: {e}", socket.display()))?;
    tokio::spawn(ctl::serve(
        listener,
        handler(Arc::clone(&host), Arc::clone(&addrs)),
    ));
    tracing::info!(socket = %socket.display(), grants = ?host.grants(), "desktopd serving");
    let device_id = onicode_desktopd::device_id(&data_dir);
    let tailnet: Arc<dyn onicode_tailnet::Tailnet> = Arc::new(onicode_tailnet::Relooking::system());
    let endpoint = loop {
        match session::bind(
            Arc::clone(&tailnet),
            &onicode_peer::SystemSockets,
            device_id.clone(),
            None,
        )
        .await
        {
            Ok(endpoint) => break endpoint,
            Err(e) => {
                tracing::info!(error = %e, "no endpoint yet: is Tailscale up?");
                tokio::time::sleep(BIND_AGAIN).await;
            }
        }
    };
    *addrs.lock() = endpoint.local_addrs();
    tracing::info!(addrs = ?endpoint.local_addrs(), "desktopd listening on the tailnet");
    let sessions = endpoint.start();
    host.serve(sessions).await;
    Ok(())
}

/// The control socket's answers.
fn handler<P: Platform, F: Factory>(
    host: Arc<Host<P, F>>,
    addrs: Arc<Mutex<Vec<SocketAddr>>>,
) -> ctl::Handler {
    Arc::new(move |request: CtlRequest| match request {
        CtlRequest::Status => CtlReply::Status(host.status(addrs.lock().clone())),
        CtlRequest::GuideOpen { pane } => {
            let leaving = Arc::clone(&host);
            match grants::guide_open(pane, move || before_exit(&*leaving)) {
                Ok(()) => CtlReply::Ok,
                Err(why) => CtlReply::Error(why),
            }
        }
    })
}

/// desktopd is about to exit to be started again: let go of every key and button its clients
/// hold down, and close their links so they hear it.
fn before_exit<P: Platform, F: Factory>(host: &Host<P, F>) {
    #[cfg(target_os = "macos")]
    if !onicode_desktop_input::let_go_everywhere(Duration::from_millis(200)) {
        tracing::warn!("some held input was not let go of before the restart");
    }
    host.close_all();
}

/// The display holder's whole life (`onicode-desktopd --display-holder <socket> <list>`): its
/// displays on the main thread, desktopds served from a thread beside it, until it is alone for
/// [`onicode_desktopd::holder::ALONE_FOR`] or asked to quit.
#[cfg(target_os = "macos")]
fn holder_main(args: &[String]) -> ExitCode {
    use onicode_desktopd::holder::{self, Ended, Holder};
    use onicode_desktopd::sized::Main;

    let (Some(socket), Some(store)) = (args.get(1), args.get(2)) else {
        return ExitCode::FAILURE;
    };
    let (socket, store) = (PathBuf::from(socket), PathBuf::from(store));
    let Some(queue) = sized::MainQueue::new(Holder::new(sized::Cg, store.clone())) else {
        return ExitCode::FAILURE;
    };
    let main: Arc<dyn Main<Holder<sized::Cg>>> = Arc::new(queue);
    let serving = std::thread::Builder::new()
        .name("onicode-display-holder".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let ended = runtime.map(|runtime| {
                runtime.block_on(async {
                    let first = holder::first_link()
                        .inspect_err(|e| tracing::info!(error = %e, "no desktopd on stdin"))
                        .ok();
                    let listener = holder::listen(&socket)
                        .inspect_err(|e| tracing::warn!(error = %e, "the holder's socket"))
                        .ok();
                    let restoring = Arc::clone(&main);
                    tokio::spawn(async move { holder::restore(restoring, &store).await });
                    let ended =
                        holder::serve(Arc::clone(&main), first, listener, holder::ALONE_FOR).await;
                    if ended == Ended::Alone {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        main.run(Box::new(move |held: &mut Holder<sized::Cg>| {
                            held.release_all();
                            let _done = tx.send(());
                        }));
                        let _released = rx.await;
                    }
                    tracing::info!(?ended, "display holder ending");
                });
            });
            #[expect(
                clippy::exit,
                reason = "the main thread never returns; this ends the process"
            )]
            std::process::exit(i32::from(ended.is_err()));
        });
    if serving.is_err() {
        return ExitCode::FAILURE;
    }
    sized::park_main()
}
