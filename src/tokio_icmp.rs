// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use icmp_socket2::Icmpv4Packet;
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;
use tokio::io::unix::AsyncFd;

const DEFAULT_RECV_BUFFER_SIZE: usize = 2048;
const DEFAULT_MAX_HOPS: u32 = 50;

/// Tokio readiness adapter matching `icmp_socket2::IcmpSocket4`'s raw IPv4 setup.
pub struct AsyncIcmpSocket4 {
    inner: AsyncFd<UdpSocket>,
    recv_buffer: Vec<u8>,
    timeout: Option<Duration>,
    max_hops: u32,
}

impl AsyncIcmpSocket4 {
    pub fn new() -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4))?;
        socket.set_nonblocking(true)?;
        let socket = UdpSocket::from(socket);

        Ok(Self {
            inner: AsyncFd::new(socket)?,
            recv_buffer: vec![0; DEFAULT_RECV_BUFFER_SIZE],
            timeout: None,
            max_hops: DEFAULT_MAX_HOPS,
        })
    }

    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }

    pub async fn send_to(&mut self, dest: Ipv4Addr, packet: Icmpv4Packet) -> io::Result<()> {
        self.inner.get_ref().set_ttl(self.max_hops)?;
        let dest = SocketAddr::new(IpAddr::V4(dest), 0);
        let bytes = packet.with_checksum().get_bytes(true);

        loop {
            let mut guard = self.inner.writable().await?;
            match guard.try_io(|inner| inner.get_ref().send_to(&bytes, dest)) {
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(error)) => return Err(error),
                Err(_) => continue,
            }
        }
    }

    pub async fn rcv_from(&mut self) -> io::Result<(Icmpv4Packet, SocketAddr)> {
        match self.timeout {
            Some(timeout) => tokio::time::timeout(timeout, self.recv())
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "rcv_from timed out"))?,
            None => self.recv().await,
        }
    }

    async fn recv(&mut self) -> io::Result<(Icmpv4Packet, SocketAddr)> {
        loop {
            let mut guard = self.inner.readable().await?;
            match guard.try_io(|inner| inner.get_ref().recv_from(&mut self.recv_buffer)) {
                Ok(Ok((read_count, addr))) => {
                    if read_count == self.recv_buffer.len() {
                        return Err(io::Error::other(
                            "received packet filled the read buffer and may have been truncated; \
                             increase it with set_read_buffer_size",
                        ));
                    }

                    let packet = Icmpv4Packet::parse_auto(&self.recv_buffer[..read_count])?;
                    return Ok((packet, addr));
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => continue,
            }
        }
    }
}
