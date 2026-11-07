//! Apple's batched datapath: the private `sendmsg_x` and `recvmsg_x` calls, which every macOS
//! this crate runs on (27.2 and later) exports from `libsystem_kernel`, so they are linked
//! directly. onicode builds this on macOS alone: iOS apps may not call private API.

use std::{
    io::{self, IoSliceMut},
    mem::{self, MaybeUninit},
    os::fd::AsRawFd,
};

use socket2::SockRef;

use crate::{
    RecvMeta, Transmit, UdpSocketState,
    cmsg::{self, MsgHdr},
    imp::{BATCH_SIZE, decode_recv, encode_control, retry_if_interrupted, send_single},
};

// Declared in xnu's bsd/sys/socket_private.h.
unsafe extern "C" {
    fn sendmsg_x(
        s: libc::c_int,
        msgp: *const MsgHdrX,
        cnt: libc::c_uint,
        flags: libc::c_int,
    ) -> isize;
    fn recvmsg_x(
        s: libc::c_int,
        msgp: *mut MsgHdrX,
        cnt: libc::c_uint,
        flags: libc::c_int,
    ) -> isize;
}

/// Sends what the socket takes of `transmit` in one call: how many of its datagrams went.
pub(crate) fn send_partial(
    state: &UdpSocketState,
    io: SockRef<'_>,
    transmit: &Transmit<'_>,
) -> io::Result<usize> {
    if state.is_apple_fast_path_enabled() {
        send_via_sendmsg_x(state, io, transmit)
    } else {
        send_single(state, io, transmit).map(|()| 1)
    }
}

/// Send using the fast `sendmsg_x` API, which can take fewer messages than it is given: the
/// count it returns is how many went, and the rest are the caller's to send again.
fn send_via_sendmsg_x(
    state: &UdpSocketState,
    io: SockRef<'_>,
    transmit: &Transmit<'_>,
) -> io::Result<usize> {
    let mut hdrs = unsafe { mem::zeroed::<[MsgHdrX; BATCH_SIZE]>() };
    let mut iovs = unsafe { mem::zeroed::<[libc::iovec; BATCH_SIZE]>() };
    let mut ctrls = [cmsg::Aligned([0u8; cmsg::LEN]); BATCH_SIZE];
    let addr = socket2::SockAddr::from(transmit.destination);
    let segment_size = transmit.segment_size.unwrap_or(transmit.contents.len());
    let mut cnt = 0;
    for (i, chunk) in transmit
        .contents
        .chunks(segment_size)
        .enumerate()
        .take(BATCH_SIZE)
    {
        prepare_msg_x(
            &Transmit {
                destination: transmit.destination,
                ecn: transmit.ecn,
                contents: chunk,
                segment_size: Some(chunk.len()),
                src_ip: transmit.src_ip,
            },
            &addr,
            &mut hdrs[i],
            &mut iovs[i],
            &mut ctrls[i],
        );
        hdrs[i].msg_datalen = chunk.len();
        state.check_send_buffer_limit(chunk.len(), &hdrs[i])?;
        cnt += 1;
    }
    let sent = retry_if_interrupted(|| unsafe {
        sendmsg_x(io.as_raw_fd(), hdrs.as_ptr(), cnt as u32, 0)
    })? as usize;
    if sent == 0 {
        return Err(io::ErrorKind::WriteZero.into());
    }
    debug_assert!(sent <= cnt);
    Ok(sent)
}

/// Prepares an `msghdr_x` for use with `sendmsg_x`.
fn prepare_msg_x(
    transmit: &Transmit<'_>,
    dst_addr: &socket2::SockAddr,
    hdr: &mut MsgHdrX,
    iov: &mut libc::iovec,
    ctrl: &mut cmsg::Aligned<[u8; cmsg::LEN]>,
) {
    iov.iov_base = transmit.contents.as_ptr() as *const _ as *mut _;
    iov.iov_len = transmit.contents.len();

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

/// Receive using the fast `recvmsg_x` API.
pub(crate) fn recv_via_recvmsg_x(
    io: SockRef<'_>,
    bufs: &mut [IoSliceMut<'_>],
    meta: &mut [RecvMeta],
) -> io::Result<usize> {
    let mut names = [MaybeUninit::<libc::sockaddr_storage>::uninit(); BATCH_SIZE];
    // `recvmsg_x` has been seen (macOS 10.15) not to override the `msghdr_x`
    // `msg_controllen`. Thus, after the call to `recvmsg_x`, one does not know
    // which control messages have been written to. To prevent reading
    // uninitialized memory, do not use `MaybeUninit` for `ctrls`, instead
    // initialize `ctrls` with `0`s. A control message of all `0`s is
    // automatically skipped by `libc::CMSG_NXTHDR`.
    let mut ctrls = [cmsg::Aligned([0u8; cmsg::LEN]); BATCH_SIZE];
    let mut hdrs = unsafe { mem::zeroed::<[MsgHdrX; BATCH_SIZE]>() };
    let max_msg_count = bufs.len().min(BATCH_SIZE);
    for i in 0..max_msg_count {
        prepare_recv_x(&mut bufs[i], &mut names[i], &mut ctrls[i], &mut hdrs[i]);
    }
    let msg_count = retry_if_interrupted(|| unsafe {
        recvmsg_x(io.as_raw_fd(), hdrs.as_mut_ptr(), max_msg_count as _, 0)
    })?;
    for i in 0..(msg_count as usize) {
        meta[i] = decode_recv(&names[i], &hdrs[i], hdrs[i].msg_datalen as usize)?;
    }
    Ok(msg_count as usize)
}

/// Prepares an `msghdr_x` for receiving with `recvmsg_x`.
fn prepare_recv_x(
    buf: &mut IoSliceMut<'_>,
    name: &mut MaybeUninit<libc::sockaddr_storage>,
    ctrl: &mut cmsg::Aligned<[u8; cmsg::LEN]>,
    hdr: &mut MsgHdrX,
) {
    hdr.msg_name = name.as_mut_ptr() as _;
    hdr.msg_namelen = size_of::<libc::sockaddr_storage>() as _;
    hdr.msg_iov = buf as *mut IoSliceMut<'_> as *mut libc::iovec;
    hdr.msg_iovlen = 1;
    hdr.msg_control = ctrl.0.as_mut_ptr() as _;
    hdr.msg_controllen = cmsg::LEN as _;
    hdr.msg_flags = 0;
    hdr.msg_datalen = buf.len();
}

// Adapted from https://github.com/apple-oss-distributions/xnu/blob/8d741a5de7ff4191bf97d57b9f54c2f6d4a15585/bsd/sys/socket_private.h
#[repr(C)]
struct MsgHdrX {
    msg_name: *mut libc::c_void,
    msg_namelen: libc::socklen_t,
    msg_iov: *mut libc::iovec,
    msg_iovlen: libc::c_int,
    msg_control: *mut libc::c_void,
    msg_controllen: libc::socklen_t,
    msg_flags: libc::c_int,
    msg_datalen: usize,
}

impl MsgHdr for MsgHdrX {
    type ControlMessage = libc::cmsghdr;

    fn cmsg_first_hdr(&self) -> *mut Self::ControlMessage {
        let selfp = self as *const _ as *mut libc::msghdr;
        unsafe { libc::CMSG_FIRSTHDR(selfp) }
    }

    fn cmsg_nxt_hdr(&self, cmsg: &Self::ControlMessage) -> *mut Self::ControlMessage {
        let selfp = self as *const _ as *mut libc::msghdr;
        unsafe { libc::CMSG_NXTHDR(selfp, cmsg) }
    }

    fn set_control_len(&mut self, len: usize) {
        self.msg_controllen = len as _;
    }

    fn control_len(&self) -> usize {
        self.msg_controllen as _
    }
}
