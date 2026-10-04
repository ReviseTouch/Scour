//! Whose process is on the other end of a loopback connection.
//!
//! The token keeps other web pages out. It does not keep other people on this
//! machine out once they have read it, and a browser's command line —
//! `--app=http://127.0.0.1:7621/?t=…` — is readable by every account. The
//! kernel knows which account owns each socket, so a connection from anyone
//! else's process is refused before its request is read.

use std::net::SocketAddr;

/// The account that owns the socket at `peer` connected to `local`, or `None`
/// when the kernel cannot be asked. One exact `sock_diag` lookup: the table in
/// `/proc/net/tcp` costs 1.5 ms a read here and grows with every socket.
#[cfg(target_os = "linux")]
pub fn owner(peer: SocketAddr, local: SocketAddr) -> Option<u32> {
    let (SocketAddr::V4(peer), SocketAddr::V4(local)) = (peer, local) else {
        return None;
    };
    const SOCK_DIAG_BY_FAMILY: u16 = 20;
    const NETLINK_SOCK_DIAG: libc::c_int = 4;
    const TCP_ESTABLISHED: u32 = 1;

    // nlmsghdr (16) + inet_diag_req_v2 (8) + inet_diag_sockid (48).
    let mut req = [0u8; 72];
    req[0..4].copy_from_slice(&72u32.to_ne_bytes());
    req[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    req[6..8].copy_from_slice(&(libc::NLM_F_REQUEST as u16).to_ne_bytes());
    req[16] = libc::AF_INET as u8;
    req[17] = libc::IPPROTO_TCP as u8;
    req[20..24].copy_from_slice(&(1u32 << TCP_ESTABLISHED).to_ne_bytes());
    // Seen from the peer's socket: its own address is the source.
    req[24..26].copy_from_slice(&peer.port().to_be_bytes());
    req[26..28].copy_from_slice(&local.port().to_be_bytes());
    req[28..32].copy_from_slice(&peer.ip().octets());
    req[44..48].copy_from_slice(&local.ip().octets());
    // No cookie: matched on the four-tuple alone.
    req[64..72].copy_from_slice(&[0xff; 8]);

    // SAFETY: a socket this function owns and closes; the buffers outlive the
    // calls, and their lengths are the ones passed.
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            NETLINK_SOCK_DIAG,
        );
        if fd < 0 {
            return None;
        }
        let mut to: libc::sockaddr_nl = std::mem::zeroed();
        to.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        let sent = libc::sendto(
            fd,
            req.as_ptr().cast(),
            req.len(),
            0,
            (&raw const to).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        );
        let mut buf = [0u8; 512];
        let got = if sent == req.len() as isize {
            libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0)
        } else {
            -1
        };
        libc::close(fd);
        let got = usize::try_from(got).ok()?;
        uid_in(&buf[..got])
    }
}

#[cfg(not(target_os = "linux"))]
pub fn owner(_peer: SocketAddr, _local: SocketAddr) -> Option<u32> {
    None
}

/// The `idiag_uid` of the one `inet_diag_msg` in a reply, or nothing for an
/// error or a reply too short to hold one.
#[cfg(target_os = "linux")]
fn uid_in(reply: &[u8]) -> Option<u32> {
    const SOCK_DIAG_BY_FAMILY: u16 = 20;
    let kind = u16::from_ne_bytes(reply.get(4..6)?.try_into().ok()?);
    if kind != SOCK_DIAG_BY_FAMILY {
        return None;
    }
    // nlmsghdr (16), then family/state/timer/retrans (4), the sockid (48),
    // expires, rqueue and wqueue (12): the uid is at 80.
    Some(u32::from_ne_bytes(reply.get(80..84)?.try_into().ok()?))
}

/// This process's own account.
#[cfg(unix)]
pub fn me() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(not(unix))]
pub fn me() -> Option<u32> {
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn a_connection_from_this_process_is_this_accounts() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let local = listener.local_addr().expect("addr");
        let _client = TcpStream::connect(local).expect("connect");
        let (served, peer) = listener.accept().expect("accept");
        assert_eq!(served.local_addr().expect("local"), local);
        assert_eq!(owner(peer, local), me());
        assert!(me().is_some());
    }

    #[test]
    fn a_connection_that_does_not_exist_has_no_owner() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let local = listener.local_addr().expect("addr");
        let nobody: SocketAddr = "127.0.0.1:9".parse().expect("addr");
        assert_eq!(owner(nobody, local), None);
    }
}
