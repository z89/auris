//! The L2CAP transport under the AAP session.
//!
//! [`AapSocket`] exists so the raw-`libc` implementation can be dropped in when
//! `bluer`'s BR/EDR path misbehaves (`AURISD_RAW_SOCKET=1`). Both are
//! SEQPACKET: one `send` is one message, one `recv` is one message.

use std::{
    future::Future,
    io,
    mem::size_of,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

use bluer::{
    l2cap::{SeqPacket, SocketAddr as L2capAddr},
    Address, AddressType,
};
use tokio::io::{unix::AsyncFd, Interest};

/// A connected AAP transport.
pub trait AapSocket: Send + Sync {
    /// Send one datagram.
    fn send(&self, buf: &[u8]) -> impl Future<Output = io::Result<usize>> + Send;
    /// Receive one datagram. A zero-length result means the peer hung up.
    fn recv(&self, buf: &mut [u8]) -> impl Future<Output = io::Result<usize>> + Send;
}

/// Whichever transport was selected at dial time.
#[derive(Debug)]
pub enum Link {
    /// `bluer`'s L2CAP SEQPACKET socket. Default.
    Bluer(SeqPacket),
    /// Raw `libc` socket fallback, selected by `AURISD_RAW_SOCKET=1`.
    Raw(RawSeqPacket),
}

impl AapSocket for Link {
    async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Bluer(s) => s.send(buf).await,
            Self::Raw(s) => s.send(buf).await,
        }
    }

    async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Bluer(s) => s.recv(buf).await,
            Self::Raw(s) => s.recv(buf).await,
        }
    }
}

/// Dial the accessory's AAP PSM. `local` is the adapter address to bind to;
/// `Address::any()` lets the kernel choose.
pub async fn connect(local: Address, peer: Address, psm: u16, raw: bool) -> io::Result<Link> {
    if raw {
        return RawSeqPacket::connect(local, peer, psm).await.map(Link::Raw);
    }
    let sock = bluer::l2cap::Socket::<SeqPacket>::new_seq_packet()?;
    sock.bind(L2capAddr::new(local, AddressType::BrEdr, 0))?;
    let seq = sock
        .connect(L2capAddr::new(peer, AddressType::BrEdr, psm))
        .await?;
    Ok(Link::Bluer(seq))
}

// ---------------------------------------------------------------------------
// Raw libc fallback
// ---------------------------------------------------------------------------

const AF_BLUETOOTH: libc::c_int = 31;
const BTPROTO_L2CAP: libc::c_int = 0;
const BDADDR_BREDR: u8 = 0;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct SockaddrL2 {
    l2_family: u16,
    l2_psm: u16,
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

impl SockaddrL2 {
    /// The kernel stores bdaddr in reverse of display order.
    fn new(addr: Address, psm: u16) -> Self {
        let mut b: [u8; 6] = addr.into();
        b.reverse();
        Self {
            l2_family: AF_BLUETOOTH as u16,
            l2_psm: psm,
            l2_bdaddr: b,
            l2_cid: 0,
            l2_bdaddr_type: BDADDR_BREDR,
        }
    }

    fn as_ptr(&self) -> *const libc::sockaddr {
        std::ptr::from_ref(self).cast()
    }
}

/// A raw AF_BLUETOOTH/SOCK_SEQPACKET/BTPROTO_L2CAP socket driven by tokio.
#[derive(Debug)]
pub struct RawSeqPacket {
    fd: AsyncFd<OwnedFd>,
}

impl RawSeqPacket {
    async fn connect(local: Address, peer: Address, psm: u16) -> io::Result<Self> {
        // SAFETY: plain syscall with constant arguments; the returned fd is
        // immediately adopted by OwnedFd so it cannot leak.
        let raw: RawFd = unsafe {
            libc::socket(
                AF_BLUETOOTH,
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                BTPROTO_L2CAP,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh, valid, owned descriptor.
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };

        let bind_addr = SockaddrL2::new(local, 0);
        // SAFETY: `bind_addr` outlives the call and its length is exact.
        if unsafe {
            libc::bind(
                raw,
                bind_addr.as_ptr(),
                size_of::<SockaddrL2>() as libc::socklen_t,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }

        let peer_addr = SockaddrL2::new(peer, psm);
        // SAFETY: as above; a non-blocking connect returns EINPROGRESS.
        let rc = unsafe {
            libc::connect(
                raw,
                peer_addr.as_ptr(),
                size_of::<SockaddrL2>() as libc::socklen_t,
            )
        };
        let err = io::Error::last_os_error();
        if rc < 0 && err.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(err);
        }

        let fd = AsyncFd::new(owned)?;
        if rc < 0 {
            fd.writable().await?.retain_ready();
            let mut so_error: libc::c_int = 0;
            let mut len = size_of::<libc::c_int>() as libc::socklen_t;
            // SAFETY: out-params are correctly sized and live for the call.
            let rc = unsafe {
                libc::getsockopt(
                    raw,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    std::ptr::from_mut(&mut so_error).cast(),
                    &raw mut len,
                )
            };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            if so_error != 0 {
                return Err(io::Error::from_raw_os_error(so_error));
            }
        }
        Ok(Self { fd })
    }

    async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.fd
            .async_io(Interest::WRITABLE, |fd| {
                // SAFETY: `buf` is valid for `buf.len()` bytes for this call.
                let n = unsafe { libc::send(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len(), 0) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            })
            .await
    }

    async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.fd
            .async_io(Interest::READABLE, |fd| {
                // SAFETY: `buf` is valid for `buf.len()` bytes for this call.
                let n =
                    unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockaddr_l2_is_thirteen_packed_bytes_with_reversed_bdaddr() {
        assert_eq!(size_of::<SockaddrL2>(), 13);
        let sa = SockaddrL2::new("AC:DE:48:00:11:22".parse().unwrap(), 0x1001);
        assert_eq!(sa.l2_bdaddr, [0x22, 0x11, 0x00, 0x48, 0xDE, 0xAC]);
        assert_eq!({ sa.l2_psm }, 0x1001);
        assert_eq!({ sa.l2_family }, 31);
        assert_eq!(sa.l2_bdaddr_type, 0);
    }
}
