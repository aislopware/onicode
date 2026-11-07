use std::{
    io::{self, IoSliceMut},
    mem::{self, MaybeUninit},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    num::NonZeroUsize,
    os::fd::AsRawFd,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

#[cfg(feature = "fast-apple-datapath")]
use std::sync::atomic::AtomicBool;

use socket2::SockRef;

use super::{
    EcnCodepoint, IO_ERROR_LOG_INTERVAL, RecvMeta, Transmit, UdpSockRef, cmsg, log_sendmsg_error,
};

#[cfg(feature = "fast-apple-datapath")]
use super::apple_fast::{recv_via_recvmsg_x, send_partial};

/// Tokio-compatible UDP socket with some useful specializations.
///
/// Unlike a standard tokio UDP socket, this allows ECN bits to be read and written.
#[derive(Debug)]
pub struct UdpSocketState {
    last_send_error: Mutex<Instant>,

    /// Whether to use Apple's batched `sendmsg_x`/`recvmsg_x` calls.
    #[cfg(feature = "fast-apple-datapath")]
    apple_fast_path: AtomicBool,

    /// Cached `SO_SNDBUF`.
    send_buffer_size: AtomicUsize,
}

impl UdpSocketState {
    pub fn new(sock: UdpSockRef<'_>) -> io::Result<Self> {
        let io = sock.0;

        assert!(
            cmsg::LEN
                >= unsafe { libc::CMSG_SPACE(size_of::<libc::c_int>() as _) as usize }
                    + unsafe { libc::CMSG_SPACE(size_of::<libc::in6_pktinfo>() as _) as usize }
        );
        assert!(
            align_of::<libc::cmsghdr>() <= align_of::<cmsg::Aligned<[u8; 0]>>(),
            "control message buffers will be misaligned"
        );

        io.set_nonblocking(true)?;

        let addr = io.local_addr()?;
        let is_ipv4 = addr.family() == libc::AF_INET as libc::sa_family_t;

        // macOS and iOS do not support IP_RECVTOS on dual-stack sockets :(
        if (is_ipv4 || !io.only_v6()?)
            && let Err(_err) =
                set_socket_option(&*io, libc::IPPROTO_IP, libc::IP_RECVTOS, OPTION_ON)
        {
            crate::log::debug!("Ignoring error setting IP_RECVTOS on socket: {_err:?}");
        }

        if is_ipv4 {
            set_socket_option(&*io, libc::IPPROTO_IP, libc::IP_DONTFRAG, OPTION_ON)?;
            // Apple has no IP_SENDSRCADDR: IP_RECVDSTADDR names the source on a send as well.
            set_socket_option(&*io, libc::IPPROTO_IP, libc::IP_RECVDSTADDR, OPTION_ON)?;
        } else {
            // Options standardized in RFC 3542
            set_socket_option(&*io, libc::IPPROTO_IPV6, libc::IPV6_RECVPKTINFO, OPTION_ON)?;
            set_socket_option(&*io, libc::IPPROTO_IPV6, libc::IPV6_RECVTCLASS, OPTION_ON)?;
            set_socket_option(&*io, libc::IPPROTO_IPV6, libc::IPV6_DONTFRAG, OPTION_ON)?;
        }

        // Enlarge SO_SNDBUF to a safe minimum.
        if io
            .send_buffer_size()
            .is_ok_and(|cur| cur < Self::MIN_SAFE_SNDBUF)
        {
            let _ = io.set_send_buffer_size(Self::MIN_SAFE_SNDBUF);
        }

        let now = Instant::now();
        Ok(Self {
            last_send_error: Mutex::new(now.checked_sub(2 * IO_ERROR_LOG_INTERVAL).unwrap_or(now)),
            #[cfg(feature = "fast-apple-datapath")]
            apple_fast_path: AtomicBool::new(false),
            send_buffer_size: AtomicUsize::new(io.send_buffer_size().unwrap_or(usize::MAX)),
        })
    }

    /// Sends a [`Transmit`] on the given socket.
    ///
    /// This function will only ever return errors of kind [`io::ErrorKind::WouldBlock`].
    /// All other errors will be logged and converted to `Ok`.
    ///
    /// UDP transmission errors are considered non-fatal because higher-level protocols must
    /// employ retransmits and timeouts anyway in order to deal with UDP's unreliable nature.
    /// Thus, logging is most likely the only thing you can do with these errors.
    ///
    /// If you would like to handle these errors yourself, use [`UdpSocketState::try_send`]
    /// instead.
    #[deprecated(note = "silences I/O errors; use `UdpSocketState::try_send()` instead")]
    pub fn send(&self, socket: UdpSockRef<'_>, transmit: &Transmit<'_>) -> io::Result<()> {
        match send_whole(self, socket.0, transmit) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(e),
            // - EMSGSIZE is expected for MTU probes. Future work might be able to avoid these by
            //   automatically clamping the MTUD upper bound to the interface MTU.
            Err(e) if e.raw_os_error() == Some(libc::EMSGSIZE) => Ok(()),
            Err(e) => {
                log_sendmsg_error(&self.last_send_error, e, transmit);

                Ok(())
            }
        }
    }

    /// Sends a [`Transmit`] on the given socket without any additional error handling.
    pub fn try_send(&self, socket: UdpSockRef<'_>, transmit: &Transmit<'_>) -> io::Result<()> {
        send_whole(self, socket.0, transmit)
    }

    /// Sends what the socket takes of `transmit` in one call, and returns how many of its
    /// datagrams that was, at least one.
    ///
    /// That is all of them, except on Apple's batched path, where `sendmsg_x` can stop short of
    /// the batch: the caller sends the rest with [`Transmit::advance`]. Errors are those of
    /// [`Self::try_send`]; [`io::ErrorKind::WouldBlock`] means nothing was sent.
    pub fn try_send_partial(
        &self,
        socket: UdpSockRef<'_>,
        transmit: &Transmit<'_>,
    ) -> io::Result<usize> {
        send_partial(self, socket.0, transmit)
    }

    pub fn recv(
        &self,
        socket: UdpSockRef<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> io::Result<usize> {
        #[cfg(feature = "fast-apple-datapath")]
        if self.is_apple_fast_path_enabled() {
            return recv_via_recvmsg_x(socket.0, bufs, meta);
        }
        recv_single(socket.0, bufs, meta)
    }

    /// The most datagrams one [`Transmit`] may carry.
    ///
    /// [`BATCH_SIZE`] once Apple's batched path is on ([`Self::set_apple_fast_path`]), where
    /// segmentation is done here in user space and each datagram goes to `sendmsg_x` as its own
    /// message; 1 otherwise.
    #[inline]
    pub fn max_gso_segments(&self) -> NonZeroUsize {
        #[cfg(feature = "fast-apple-datapath")]
        if self.is_apple_fast_path_enabled() {
            return NonZeroUsize::new(BATCH_SIZE).expect("BATCH_SIZE is not zero");
        }
        NonZeroUsize::MIN
    }

    /// The number of segments one received buffer may hold.
    ///
    /// Used as a factor to compute the receive buffer size. Always 1: Apple platforms have no
    /// receive offload.
    #[inline]
    pub fn gro_segments(&self) -> NonZeroUsize {
        NonZeroUsize::MIN
    }

    /// Resize the send buffer of `socket` to `bytes`
    ///
    /// `bytes` is silently raised to a safe minimum if smaller.
    #[inline]
    pub fn set_send_buffer_size(&self, socket: UdpSockRef<'_>, bytes: usize) -> io::Result<()> {
        let bytes = if bytes < Self::MIN_SAFE_SNDBUF {
            crate::log::debug!(
                "raising requested SO_SNDBUF from {bytes} to {}",
                Self::MIN_SAFE_SNDBUF
            );
            Self::MIN_SAFE_SNDBUF
        } else {
            bytes
        };
        socket.0.set_send_buffer_size(bytes)?;
        self.send_buffer_size.store(
            socket.0.send_buffer_size().unwrap_or(bytes),
            Ordering::Relaxed,
        );
        Ok(())
    }

    /// Resize the receive buffer of `socket` to `bytes`
    #[inline]
    pub fn set_recv_buffer_size(&self, socket: UdpSockRef<'_>, bytes: usize) -> io::Result<()> {
        socket.0.set_recv_buffer_size(bytes)
    }

    /// Get the size of the `socket` send buffer
    #[inline]
    pub fn send_buffer_size(&self, socket: UdpSockRef<'_>) -> io::Result<usize> {
        socket.0.send_buffer_size()
    }

    /// Get the size of the `socket` receive buffer
    #[inline]
    pub fn recv_buffer_size(&self, socket: UdpSockRef<'_>) -> io::Result<usize> {
        socket.0.recv_buffer_size()
    }

    /// Whether transmitted datagrams might get fragmented by the IP layer
    ///
    /// Never: [`Self::new`] sets `IP_DONTFRAG` or `IPV6_DONTFRAG` on every socket.
    #[inline]
    pub fn may_fragment(&self) -> bool {
        false
    }

    /// Enables Apple's batched UDP datapath, the private `sendmsg_x`/`recvmsg_x` calls, which
    /// every macOS this crate runs on has.
    ///
    /// Once enabled, [`max_gso_segments`] is [`BATCH_SIZE`], so callers batch sends.
    ///
    /// [`max_gso_segments`]: Self::max_gso_segments
    #[cfg(feature = "fast-apple-datapath")]
    pub fn set_apple_fast_path(&self) {
        self.apple_fast_path.store(true, Ordering::Relaxed);
    }

    /// Returns whether Apple's fast UDP datapath is enabled for this socket.
    #[cfg(feature = "fast-apple-datapath")]
    pub fn is_apple_fast_path_enabled(&self) -> bool {
        self.apple_fast_path.load(Ordering::Relaxed)
    }

    /// Smallest `SO_SNDBUF` that mitigates <https://feedbackassistant.apple.com/feedback/23671230>:
    /// On macOS, a non-blocking SOCK_DGRAM `sendmsg`/`sendmsg_x` call with ancillary data returns
    /// `EWOULDBLOCK` when the payload length is at or just under `SO_SNDBUF`.
    const MIN_SAFE_SNDBUF: usize = 65535 + cmsg::LEN;

    /// Safety net returning `EMSGSIZE` for payload sizes in the bug's region.
    pub(crate) fn check_send_buffer_limit(
        &self,
        resid: usize,
        hdr: &impl cmsg::MsgHdr,
    ) -> io::Result<()> {
        let needed = resid.saturating_add(hdr.control_len());
        let sndbuf = self.send_buffer_size.load(Ordering::Relaxed);
        if needed > sndbuf {
            crate::log::debug!("EMSGSIZE for {needed}-byte send: exceeds SO_SNDBUF ({sndbuf})");
            return Err(io::Error::from_raw_os_error(libc::EMSGSIZE));
        }
        Ok(())
    }
}

/// Sends every datagram of `transmit`, going on after a partial send. Once some went, a socket
/// with no room for the rest drops the rest, as a full buffer drops a datagram; before that it
/// is [`io::ErrorKind::WouldBlock`], and nothing was sent.
fn send_whole(state: &UdpSocketState, io: SockRef<'_>, transmit: &Transmit<'_>) -> io::Result<()> {
    let count = transmit.datagram_count();
    let mut sent = send_partial(state, SockRef::from(&*io), transmit)?;
    while sent < count {
        match send_partial(state, SockRef::from(&*io), &transmit.advance(sent)) {
            Ok(n) => sent += n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                crate::log::debug!("dropped {} of {count} datagrams: no room", count - sent);
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// One call's worth of `transmit`: every datagram, as one `sendmsg`.
#[cfg(not(feature = "fast-apple-datapath"))]
fn send_partial(
    state: &UdpSocketState,
    io: SockRef<'_>,
    transmit: &Transmit<'_>,
) -> io::Result<usize> {
    send_single(state, io, transmit).map(|()| transmit.datagram_count())
}

pub(crate) fn send_single(
    state: &UdpSocketState,
    io: SockRef<'_>,
    transmit: &Transmit<'_>,
) -> io::Result<()> {
    let mut hdr: libc::msghdr = unsafe { mem::zeroed() };
    let mut iov: libc::iovec = unsafe { mem::zeroed() };
    let mut ctrl = cmsg::Aligned([0u8; cmsg::LEN]);
    let addr = socket2::SockAddr::from(transmit.destination);
    prepare_msg(transmit, &addr, &mut hdr, &mut iov, &mut ctrl);
    state.check_send_buffer_limit(transmit.contents.len(), &hdr)?;
    retry_if_interrupted(|| unsafe { libc::sendmsg(io.as_raw_fd(), &hdr, 0) })?;
    Ok(())
}

pub(crate) fn recv_single(
    io: SockRef<'_>,
    bufs: &mut [IoSliceMut<'_>],
    meta: &mut [RecvMeta],
) -> io::Result<usize> {
    let mut name = MaybeUninit::<libc::sockaddr_storage>::uninit();
    let mut ctrl = cmsg::Aligned(MaybeUninit::<[u8; cmsg::LEN]>::uninit());
    let mut hdr = unsafe { mem::zeroed::<libc::msghdr>() };
    prepare_recv(&mut bufs[0], &mut name, &mut ctrl, &mut hdr);
    let n = loop {
        let n = unsafe { libc::recvmsg(io.as_raw_fd(), &mut hdr, 0) };

        if hdr.msg_flags & libc::MSG_TRUNC != 0 {
            continue;
        }

        if n >= 0 {
            break n;
        }

        let e = io::Error::last_os_error();
        match e.kind() {
            // Retry receiving
            io::ErrorKind::Interrupted => continue,
            _ => return Err(e),
        }
    };
    meta[0] = decode_recv(&name, &hdr, n as usize)?;
    Ok(1)
}

fn prepare_msg(
    transmit: &Transmit<'_>,
    dst_addr: &socket2::SockAddr,
    hdr: &mut libc::msghdr,
    iov: &mut libc::iovec,
    ctrl: &mut cmsg::Aligned<[u8; cmsg::LEN]>,
) {
    iov.iov_base = transmit.contents.as_ptr() as *const _ as *mut _;
    iov.iov_len = transmit.contents.len();

    // SAFETY: Casting the pointer to a mutable one is legal,
    // as sendmsg is guaranteed to not alter the mutable pointer
    // as per the POSIX spec. See the section on the sys/socket.h
    // header for details. The type is only mutable in the first
    // place because it is reused by recvmsg as well.
    let name = dst_addr.as_ptr() as *mut libc::c_void;
    let namelen = dst_addr.len();
    hdr.msg_name = name as *mut _;
    hdr.msg_namelen = namelen;
    hdr.msg_iov = iov;
    hdr.msg_iovlen = 1;

    hdr.msg_control = ctrl.0.as_mut_ptr() as _;
    hdr.msg_controllen = cmsg::LEN as _;
    let mut encoder = unsafe { cmsg::Encoder::new(hdr) };
    encode_control(transmit, &mut encoder);
    encoder.finish();
}

/// Writes the control messages a send of `transmit` carries: its ECN bits, and its source
/// address when it names one.
pub(crate) fn encode_control<M: cmsg::MsgHdr>(
    transmit: &Transmit<'_>,
    encoder: &mut cmsg::Encoder<'_, M>,
) {
    let ecn = transmit.ecn.map_or(0, |x| x as libc::c_int);
    // True for IPv4 or IPv4-Mapped IPv6
    let is_ipv4 = transmit.destination.is_ipv4()
        || matches!(transmit.destination.ip(), IpAddr::V6(addr) if addr.to_ipv4_mapped().is_some());
    if is_ipv4 {
        encoder.push(libc::IPPROTO_IP, libc::IP_TOS, ecn);
    } else {
        encoder.push(libc::IPPROTO_IPV6, libc::IPV6_TCLASS, ecn);
    }

    if let Some(ip) = &transmit.src_ip {
        match ip {
            // Apple has no IP_SENDSRCADDR; IP_RECVDSTADDR names the source on a send.
            IpAddr::V4(v4) => {
                let addr = libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.octets()),
                };
                encoder.push(libc::IPPROTO_IP, libc::IP_RECVDSTADDR, addr);
            }
            IpAddr::V6(v6) => {
                let pktinfo = libc::in6_pktinfo {
                    ipi6_ifindex: 0,
                    ipi6_addr: libc::in6_addr {
                        s6_addr: v6.octets(),
                    },
                };
                encoder.push(libc::IPPROTO_IPV6, libc::IPV6_PKTINFO, pktinfo);
            }
        }
    }
}

fn prepare_recv(
    buf: &mut IoSliceMut<'_>,
    name: &mut MaybeUninit<libc::sockaddr_storage>,
    ctrl: &mut cmsg::Aligned<MaybeUninit<[u8; cmsg::LEN]>>,
    hdr: &mut libc::msghdr,
) {
    hdr.msg_name = name.as_mut_ptr() as _;
    hdr.msg_namelen = size_of::<libc::sockaddr_storage>() as _;
    hdr.msg_iov = buf as *mut IoSliceMut<'_> as *mut libc::iovec;
    hdr.msg_iovlen = 1;
    hdr.msg_control = ctrl.0.as_mut_ptr() as _;
    hdr.msg_controllen = cmsg::LEN as _;
    hdr.msg_flags = 0;
}

pub(crate) fn decode_recv<M: cmsg::MsgHdr<ControlMessage = libc::cmsghdr>>(
    name: &MaybeUninit<libc::sockaddr_storage>,
    hdr: &M,
    len: usize,
) -> io::Result<RecvMeta> {
    let name = unsafe { name.assume_init() };
    let mut ctrl = ControlMetadata {
        ecn_bits: 0,
        dst_ip: None,
        interface_index: None,
    };

    let cmsg_iter = unsafe { cmsg::Iter::new(hdr) };
    for cmsg in cmsg_iter {
        ctrl.decode(cmsg);
    }

    Ok(RecvMeta {
        len,
        stride: len,
        addr: decode_socket_addr(&name)?,
        ecn: EcnCodepoint::from_bits(ctrl.ecn_bits),
        dst_ip: ctrl.dst_ip,
        interface_index: ctrl.interface_index,
    })
}

/// Metadata decoded from control messages
struct ControlMetadata {
    ecn_bits: u8,
    dst_ip: Option<IpAddr>,
    interface_index: Option<u32>,
}

impl ControlMetadata {
    /// Decodes a control message and updates the metadata state
    fn decode(&mut self, cmsg: &libc::cmsghdr) {
        match (cmsg.cmsg_level, cmsg.cmsg_type) {
            (libc::IPPROTO_IP, libc::IP_TOS) => unsafe {
                self.ecn_bits = cmsg::decode::<u8, libc::cmsghdr>(cmsg);
            },
            (libc::IPPROTO_IP, libc::IP_RECVTOS) => unsafe {
                self.ecn_bits = cmsg::decode::<u8, libc::cmsghdr>(cmsg);
            },
            (libc::IPPROTO_IPV6, libc::IPV6_TCLASS) => unsafe {
                // Temporary hack around broken macos ABI. Remove once upstream fixes it.
                // https://bugreport.apple.com/web/?problemID=48761855
                if cmsg.cmsg_len as usize == libc::CMSG_LEN(size_of::<u8>() as _) as usize {
                    self.ecn_bits = cmsg::decode::<u8, libc::cmsghdr>(cmsg);
                } else {
                    self.ecn_bits = cmsg::decode::<libc::c_int, libc::cmsghdr>(cmsg) as u8;
                }
            },
            (libc::IPPROTO_IP, libc::IP_RECVDSTADDR) => {
                let in_addr = unsafe { cmsg::decode::<libc::in_addr, libc::cmsghdr>(cmsg) };
                self.dst_ip = Some(IpAddr::V4(Ipv4Addr::from(in_addr.s_addr.to_ne_bytes())));
            }
            (libc::IPPROTO_IPV6, libc::IPV6_PKTINFO) => {
                let pktinfo = unsafe { cmsg::decode::<libc::in6_pktinfo, libc::cmsghdr>(cmsg) };
                self.dst_ip = Some(IpAddr::V6(Ipv6Addr::from(pktinfo.ipi6_addr.s6_addr)));
                self.interface_index = Some(pktinfo.ipi6_ifindex);
            }
            _ => {}
        }
    }
}

/// Decodes a `sockaddr_storage` into a `SocketAddr`
fn decode_socket_addr(name: &libc::sockaddr_storage) -> io::Result<SocketAddr> {
    match libc::c_int::from(name.ss_family) {
        libc::AF_INET => {
            // Safety: if the ss_family field is AF_INET then storage must be a sockaddr_in.
            let addr: &libc::sockaddr_in =
                unsafe { &*(name as *const _ as *const libc::sockaddr_in) };
            Ok(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes()),
                u16::from_be(addr.sin_port),
            )))
        }
        libc::AF_INET6 => {
            // Safety: if the ss_family field is AF_INET6 then storage must be a sockaddr_in6.
            let addr: &libc::sockaddr_in6 =
                unsafe { &*(name as *const _ as *const libc::sockaddr_in6) };
            Ok(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(addr.sin6_addr.s6_addr),
                u16::from_be(addr.sin6_port),
                addr.sin6_flowinfo,
                addr.sin6_scope_id,
            )))
        }
        f => Err(io::Error::other(format!(
            "expected AF_INET or AF_INET6, got {f}"
        ))),
    }
}

/// Datagrams one batched `sendmsg_x`/`recvmsg_x` call carries. Chosen somewhat arbitrarily;
/// might benefit from additional tuning.
#[cfg(feature = "fast-apple-datapath")]
pub(crate) const BATCH_SIZE: usize = 32;

/// One datagram a call: without the batched path every send and receive is a `sendmsg` or a
/// `recvmsg`.
#[cfg(not(feature = "fast-apple-datapath"))]
pub(crate) const BATCH_SIZE: usize = 1;

fn set_socket_option(
    socket: &impl AsRawFd,
    level: libc::c_int,
    name: libc::c_int,
    value: libc::c_int,
) -> io::Result<()> {
    let rc = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            level,
            name,
            &value as *const _ as _,
            size_of_val(&value) as _,
        )
    };

    match rc == 0 {
        true => Ok(()),
        false => Err(io::Error::last_os_error()),
    }
}

const OPTION_ON: libc::c_int = 1;

/// Calls `f` in a loop, retrying on `EINTR`.
///
/// Returns the non-negative result or the first non-`EINTR` error.
pub(crate) fn retry_if_interrupted(mut f: impl FnMut() -> isize) -> io::Result<isize> {
    loop {
        let n = f();
        if n >= 0 {
            return Ok(n);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}
