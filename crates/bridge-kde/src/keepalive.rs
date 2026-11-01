//! Dead-link detection timers for link sockets (docs/chaos-findings.md, C-2 / C-5).
//!
//! Idle 30 s, then probes every 10 s, three of them: a vanished peer is noticed in about 60 s.
//! Linux and macOS take all three from socket2; Linux also gets `TCP_USER_TIMEOUT`. Windows needs
//! more: socket2 sets idle and interval through the `SIO_KEEPALIVE_VALS` ioctl but cannot set the
//! probe count, and the OS default of 10 probes would stretch detection to about 130 s.

use std::time::Duration;

pub(crate) const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
pub(crate) const KEEPALIVE_PROBES: u32 = 3;
/// Longest a vanished peer may go unnoticed.
pub(crate) const DEAD_LINK_LIMIT: Duration = Duration::from_secs(60);

/// Arm the keepalive timer (and the platform's hard limit) on a link socket.
pub(crate) fn arm(sock: &socket2::SockRef<'_>) -> std::io::Result<()> {
    let ka = socket2::TcpKeepalive::new().with_time(KEEPALIVE_IDLE);
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    let ka = ka
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_PROBES);
    // On Windows socket2 issues SIO_KEEPALIVE_VALS with the idle time and the interval.
    #[cfg(windows)]
    let ka = ka.with_interval(KEEPALIVE_INTERVAL);
    sock.set_tcp_keepalive(&ka)?;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    sock.set_tcp_user_timeout(Some(DEAD_LINK_LIMIT))?;
    #[cfg(windows)]
    windows_limits(sock);
    Ok(())
}

/// Windows 10 1703+: probe count and the retransmission limit for unacknowledged data. Older
/// Windows rejects the options; the ioctl timers above still apply, so this is best effort.
#[cfg(windows)]
fn windows_limits(sock: &socket2::SockRef<'_>) {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        IPPROTO_TCP, SOCKET, TCP_KEEPCNT, TCP_MAXRT, setsockopt,
    };
    let raw = sock.as_raw_socket() as SOCKET;
    let set = |name: i32, value: u32| {
        // SAFETY: `raw` is a live socket owned by the caller for the duration of the call, and
        // `value` is a 4-byte DWORD, which is what both options expect.
        unsafe {
            setsockopt(
                raw,
                IPPROTO_TCP,
                name,
                std::ptr::addr_of!(value).cast::<u8>(),
                std::mem::size_of::<u32>() as i32,
            )
        }
    };
    let _ = set(TCP_KEEPCNT, KEEPALIVE_PROBES);
    let _ = set(TCP_MAXRT, DEAD_LINK_LIMIT.as_secs() as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_budget_detects_a_dead_link_within_a_minute() {
        let worst = KEEPALIVE_IDLE + KEEPALIVE_INTERVAL * KEEPALIVE_PROBES;
        assert!(worst <= DEAD_LINK_LIMIT, "{worst:?}");
    }

    #[test]
    fn arming_an_ordinary_socket_succeeds() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let c = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
        arm(&socket2::SockRef::from(&c)).unwrap();
        assert!(socket2::SockRef::from(&c).keepalive().unwrap());
    }
}
