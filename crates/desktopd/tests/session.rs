//! desktopd's sessions on the simulated tailnet, on the drawn platform: the grants gate, a
//! client of another wire, and two of the owner's devices at once, one holding the controls,
//! each with a stream of its own, where only the controller's input reaches the Mac and the
//! other is told once that it does not hold them.
//!
//! The drawn platform runs on the runtime, so each test runs on a paused clock: its streams, the
//! network and every wait are virtual time, and its drawn Mac is its own
//! (`onicode_desktopd::synthetic::recorded`).

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use onicode_desktop_link::{Down, HostGrants, PORT, Pane, Up, WIRE};
use onicode_desktop_proto::screen::{
    CaptureTarget, Quality, ScreenEvent, ScreenInput, ScreenRequest,
};
use onicode_desktop_proto::{DisplayId, StreamId};
use onicode_desktopd::session::{self, Host};
use onicode_desktopd::sized;
use onicode_desktopd::synthetic::{self, Drawn};
use onicode_net::transport::endpoint_on;
use onicode_net::{Role, StreamRx, StreamTx};
use onicode_net_sim::tailnet::ipv4;
use onicode_net_sim::{Faults, Net, NodeId, NodeSpec, SimTailnet, report_seed};
use onicode_peer::{PeerConfig, PeerEndpoint, PeerSession, Sockets};
use tokio::sync::watch;

const USER: i64 = 2;
/// The longest any one answer may take, in virtual time.
const PATIENCE: Duration = Duration::from_secs(5);

/// The simulated network's sockets.
struct SimSockets(Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

const ALL: HostGrants = HostGrants {
    screen_recording: true,
    accessibility: true,
    post_event: true,
};

/// desktopd on the node `mac`, its grants as the test sets them.
struct Mac {
    tailnet: SimTailnet,
    node: NodeId,
    grants: watch::Sender<HostGrants>,
    host: Arc<Host<Drawn, sized::Cg>>,
    _endpoint: Arc<PeerEndpoint>,
}

async fn mac(seed: u64, grants: HostGrants) -> Mac {
    let tailnet = SimTailnet::new(Faults::CLEAR, seed);
    let node = tailnet.add(NodeSpec::owned("mac", USER));
    let (grants, read) = watch::channel(grants);
    let host = Host::<Drawn, sized::Cg>::new("mac".to_owned(), read, None);
    let endpoint = session::bind(
        tailnet.local_api(node),
        &SimSockets(tailnet.net().clone()),
        "dev-mac-desktop".to_owned(),
        Some(seed),
    )
    .await
    .expect("desktopd binds its tailnet address");
    tokio::spawn(Arc::clone(&host).serve(endpoint.start()));
    Mac {
        tailnet,
        node,
        grants,
        host,
        _endpoint: endpoint,
    }
}

/// One of the owner's devices, linked to desktopd.
struct Client {
    tx: StreamTx,
    rx: StreamRx,
    _endpoint: Arc<PeerEndpoint>,
}

impl Mac {
    async fn client(&self, name: &str, seed: u64) -> Client {
        let node = self.tailnet.add(NodeSpec::owned(name, USER));
        let endpoint = PeerEndpoint::bind(
            self.tailnet.local_api(node),
            &SimSockets(self.tailnet.net().clone()),
            PeerConfig::new(format!("dev-{name}"), Role::Viewer).with_seed(seed),
        )
        .await
        .expect("the client binds");
        let PeerSession { session, admitted } = endpoint
            .dial(SocketAddr::new(ipv4(self.node), PORT))
            .await
            .expect("desktopd lets the owner's device in");
        assert!(admitted.grant.owner);
        Client {
            tx: session.control_tx,
            rx: session.control_rx,
            _endpoint: endpoint,
        }
    }
}

impl Client {
    fn say(&self, up: &Up) {
        self.tx
            .send_frame(&onicode_desktop_link::encode(up))
            .expect("the link is up");
    }

    fn join(&self, name: &str, control: bool) {
        self.say(&Up::Join {
            wire: WIRE,
            name: name.to_owned(),
            control,
            sound: false,
        });
    }

    fn screen(&self, request: ScreenRequest) {
        self.say(&Up::Screen(request));
    }

    /// The next frame from desktopd; `None` once the link is closed.
    async fn next(&mut self) -> Option<Down> {
        let frame = tokio::time::timeout(PATIENCE, self.rx.frame())
            .await
            .expect("desktopd answers in time");
        match frame {
            Ok(Some(frame)) => {
                Some(onicode_desktop_link::decode(&frame).expect("a frame of the wire"))
            }
            Ok(None) | Err(_) => None,
        }
    }

    /// The frames up to and with the first `wanted` one.
    async fn until(&mut self, wanted: impl Fn(&Down) -> bool) -> (Vec<Down>, Down) {
        let mut before = Vec::new();
        loop {
            let down = self.next().await.expect("the link stays up");
            if wanted(&down) {
                return (before, down);
            }
            before.push(down);
        }
    }

    /// Open a stream of `display`; its id.
    async fn open(&mut self, display: u32) -> StreamId {
        self.screen(ScreenRequest::Open {
            target: CaptureTarget::Display(DisplayId(display)),
            quality: Quality {
                bitrate_bps: 4_000_000,
                ..Quality::default()
            },
        });
        let (_before, opened) = self
            .until(|d| {
                matches!(
                    d,
                    Down::Screen(ScreenEvent::Opened { .. } | ScreenEvent::OpenFailed { .. })
                )
            })
            .await;
        let Down::Screen(ScreenEvent::Opened { stream, target, .. }) = opened else {
            panic!("not opened: {opened:?}");
        };
        assert_eq!(target, CaptureTarget::Display(DisplayId(display)));
        stream
    }
}

fn welcomed(down: &Down) -> bool {
    matches!(down, Down::Welcome { .. })
}

fn seat_of(down: &Down) -> Option<bool> {
    match down {
        Down::Seat(seat) | Down::Welcome { seat, .. } => Some(seat.you),
        _other => None,
    }
}

/// Wait until the drawn injector was handed `input` on `display`.
async fn recorded(display: u32, input: &ScreenInput) -> bool {
    let wanted = (CaptureTarget::Display(DisplayId(display)), input.clone());
    tokio::time::timeout(PATIENCE, async {
        while !synthetic::recorded().contains(&wanted) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

/// While a grant is off, a client that joins is told what is missing and kept; each change is
/// told, and once every grant is on it is welcomed.
#[tokio::test(start_paused = true)]
async fn a_client_waits_at_the_grants_until_they_are_on() {
    const SEED: u64 = 71;
    let _seed = report_seed("a_client_waits_at_the_grants_until_they_are_on", SEED);
    let off = HostGrants {
        screen_recording: false,
        accessibility: false,
        post_event: false,
    };
    let mac = mac(SEED, off).await;
    let mut phone = mac.client("phone", SEED + 1).await;
    phone.join("phone", false);
    let Some(Down::NotPermitted { missing }) = phone.next().await else {
        panic!("not told what is missing");
    };
    assert_eq!(missing, [Pane::ScreenRecording, Pane::Accessibility]);

    let halfway = HostGrants {
        screen_recording: true,
        ..off
    };
    mac.grants.send_replace(halfway);
    assert_eq!(phone.next().await, Some(Down::Grants(halfway)));
    mac.grants.send_replace(ALL);
    assert_eq!(phone.next().await, Some(Down::Grants(ALL)));
    let welcome = phone.next().await.expect("welcomed");
    let Down::Welcome {
        wire,
        host,
        displays,
        grants,
        seat,
    } = welcome
    else {
        panic!("not welcomed: {welcome:?}");
    };
    assert_eq!((wire, host.as_str(), grants), (WIRE, "mac", ALL));
    assert_eq!(displays, synthetic::DISPLAYS);
    assert!(!seat.you, "it did not ask for the controls");
    assert_eq!(seat.clients, 1);
    assert_eq!(mac.host.clients(), 1);
}

/// A client of another wire, or one that never joins, is closed.
#[tokio::test(start_paused = true)]
async fn a_client_of_another_wire_is_closed() {
    const SEED: u64 = 72;
    let _seed = report_seed("a_client_of_another_wire_is_closed", SEED);
    let mac = mac(SEED, ALL).await;
    let mut phone = mac.client("phone", SEED + 1).await;
    phone.say(&Up::Join {
        wire: WIRE + 1,
        name: "phone".to_owned(),
        control: false,
        sound: false,
    });
    assert_eq!(phone.next().await, None, "closed, never welcomed");

    let mut silent = mac.client("laptop", SEED + 2).await;
    tokio::time::sleep(session::JOIN_WITHIN).await;
    assert_eq!(
        silent.next().await,
        None,
        "closed once it was quiet too long"
    );
}

/// Two of the owner's devices at once, each streaming a display of its own: the one that asked
/// for the controls drives the Mac, the other is told once that it does not, until it takes
/// them, when the first is told in turn.
#[tokio::test(start_paused = true)]
async fn only_the_controller_drives_and_a_viewer_is_told_once() {
    const SEED: u64 = 73;
    let _seed = report_seed("only_the_controller_drives_and_a_viewer_is_told_once", SEED);
    let mac = mac(SEED, ALL).await;

    let mut laptop = mac.client("laptop", SEED + 1).await;
    laptop.join("laptop", true);
    let (_before, welcome) = laptop.until(welcomed).await;
    assert_eq!(
        seat_of(&welcome),
        Some(true),
        "the owner's device that asked holds them"
    );

    let mut phone = mac.client("phone", SEED + 2).await;
    phone.join("phone", false);
    let (_before, welcome) = phone.until(welcomed).await;
    assert_eq!(seat_of(&welcome), Some(false));
    let Down::Welcome { seat, .. } = &welcome else {
        unreachable!()
    };
    assert_eq!(seat.controller.as_deref(), Some("laptop"));
    assert_eq!(seat.clients, 2);
    // The laptop hears the phone come.
    let (_before, seat) = laptop
        .until(|d| matches!(d, Down::Seat(s) if s.clients == 2))
        .await;
    assert_eq!(seat_of(&seat), Some(true));

    let on_laptop = laptop.open(1).await;
    let on_phone = phone.open(2).await;

    // The viewer's input is refused, and it is told so once.
    let phone_move = ScreenInput::Move { x: 3.0, y: 5.0 };
    phone.screen(ScreenRequest::Input {
        stream: on_phone,
        input: phone_move.clone(),
    });
    let (_before, refused) = phone.until(|d| matches!(d, Down::NotController)).await;
    assert_eq!(refused, Down::NotController);
    phone.screen(ScreenRequest::Input {
        stream: on_phone,
        input: phone_move.clone(),
    });
    // Letting go of controls it does not hold changes nothing and is answered with its seat,
    // after whatever the second input was answered with.
    phone.say(&Up::Control { take: false });
    let (before, seat) = phone.until(|d| matches!(d, Down::Seat(_))).await;
    assert_eq!(seat_of(&seat), Some(false));
    assert!(
        !before.contains(&Down::NotController),
        "told once, not twice: {before:?}"
    );

    // The controller's reaches the Mac.
    let laptop_move = ScreenInput::Move { x: 41.0, y: 17.0 };
    laptop.screen(ScreenRequest::Input {
        stream: on_laptop,
        input: laptop_move.clone(),
    });
    assert!(
        recorded(1, &laptop_move).await,
        "the controller's input is injected"
    );
    assert!(
        !synthetic::recorded().contains(&(CaptureTarget::Display(DisplayId(2)), phone_move)),
        "the viewer's never was"
    );

    // The phone takes the controls; the laptop is told, and its input is refused from now on.
    phone.say(&Up::Control { take: true });
    let (_before, seat) = phone.until(|d| seat_of(d) == Some(true)).await;
    let Down::Seat(seat) = seat else {
        unreachable!()
    };
    assert_eq!(seat.controller.as_deref(), Some("phone"));
    let (_before, _lost) = laptop.until(|d| seat_of(d) == Some(false)).await;
    laptop.screen(ScreenRequest::Input {
        stream: on_laptop,
        input: laptop_move.clone(),
    });
    let (_before, refused) = laptop.until(|d| matches!(d, Down::NotController)).await;
    assert_eq!(refused, Down::NotController);
    let phone_drives = ScreenInput::Move { x: 7.0, y: 9.0 };
    phone.screen(ScreenRequest::Input {
        stream: on_phone,
        input: phone_drives.clone(),
    });
    assert!(
        recorded(2, &phone_drives).await,
        "the new controller drives its display"
    );
    assert_eq!(mac.host.clients(), 2);
}
