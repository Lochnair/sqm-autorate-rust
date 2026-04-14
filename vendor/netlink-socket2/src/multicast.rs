use std::{io, os::fd::AsRawFd, sync::Arc};

use netlink_bindings::utils;

use crate::{NetlinkReplyInner, NetlinkSocket, ReplyError, Socket, RECV_BUF_SIZE};

#[derive(Debug, Clone)]
pub struct MulticastRecv {
    pub multicast_group: u32,
    pub message_type: u16,
}

pub struct MulticastSocketRaw {
    buf: Arc<[u8; RECV_BUF_SIZE]>,
    sock: Socket,
    reply: NetlinkReplyInner,
    last_group: Option<u32>,
}

impl MulticastSocketRaw {
    pub fn new(protonum: u16) -> io::Result<Self> {
        let sock = NetlinkSocket::get_socket_new(protonum)?;

        // Enable multicast group number via recvmsg ancillary messages
        let res = unsafe {
            libc::setsockopt(
                sock.as_raw_fd(),
                libc::SOL_NETLINK,
                libc::NETLINK_PKTINFO,
                &1u32 as *const u32 as *const libc::c_void,
                4,
            )
        };
        if res < 0 {
            return Err(io::Error::from_raw_os_error(-res));
        }

        let mut buf: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        buf.nl_family = libc::AF_NETLINK as u16;
        buf.nl_groups = 0;

        let res = unsafe {
            libc::bind(
                sock.as_raw_fd(),
                &buf as *const _ as *const libc::sockaddr,
                std::mem::size_of_val(&buf) as libc::socklen_t,
            )
        };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            buf: Arc::new([0u8; RECV_BUF_SIZE]),
            sock,
            reply: NetlinkReplyInner {
                buf_offset: 0,
                buf_read: 0,
            },
            last_group: None,
        })
    }

    pub fn listen(&mut self, group_id: u32) -> io::Result<()> {
        let res = unsafe {
            libc::setsockopt(
                self.sock.as_raw_fd(),
                libc::SOL_NETLINK,
                libc::NETLINK_ADD_MEMBERSHIP,
                &group_id as *const u32 as *const libc::c_void,
                4,
            )
        };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    #[cfg_attr(not(feature = "async"), maybe_async::maybe_async)]
    pub async fn recv(&mut self) -> Result<(MulticastRecv, &[u8]), ReplyError> {
        let buf = Arc::make_mut(&mut self.buf);

        loop {
            if self.reply.buf_offset == self.reply.buf_read {
                let read = Self::read_buf(&mut self.sock, buf, &mut self.last_group).await?;
                self.reply.buf_read = read;
                self.reply.buf_offset = 0;
            }

            match self.reply.parse_next(buf).await {
                Err(io_err) => {
                    return Err(io_err.into());
                }
                Ok((seq, message_type, res)) => {
                    if seq != 0 {
                        continue;
                    }

                    let Some(multicast_group) = self.last_group else {
                        continue;
                    };

                    match res {
                        Ok((l, r)) => {
                            return Ok((
                                MulticastRecv {
                                    multicast_group,
                                    message_type,
                                },
                                &self.buf[l..r],
                            ));
                        }
                        Err(mut err) => {
                            if err.code.raw_os_error().unwrap() == 0 {
                                continue;
                            }

                            if err.has_context() {
                                // err.lookup = Request::lookup;
                                err.reply_buf = Some(self.buf.clone());
                            }

                            return Err(err);
                        }
                    };
                }
            };
        }
    }

    #[cfg_attr(not(feature = "async"), maybe_async::maybe_async)]
    async fn read_buf(
        sock: &Socket,
        buf: &mut [u8],
        last_group: &mut Option<u32>,
    ) -> Result<usize, ReplyError> {
        loop {
            #[cfg(feature = "async")]
            sock.readable().await?;

            let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            let mut iov = libc::iovec {
                iov_base: buf.as_mut_ptr() as *mut libc::c_void,
                iov_len: buf.len(),
            };

            let mut control_buf = [0u8; 128];

            // Use zeroed init + field assignment to avoid private-field struct
            // literal restrictions on musl targets (__pad1, __pad2 in msghdr).
            let mut msghdr: libc::msghdr = unsafe { std::mem::zeroed() };
            msghdr.msg_name = &mut addr as *mut libc::sockaddr_nl as *mut libc::c_void;
            msghdr.msg_namelen = std::mem::size_of_val(&addr) as u32;
            msghdr.msg_iov = &mut iov as *mut libc::iovec;
            msghdr.msg_iovlen = 1;
            msghdr.msg_control = control_buf.as_mut_ptr() as *mut libc::c_void;
            // msg_controllen is usize on glibc but u32 on musl — let the compiler infer.
            msghdr.msg_controllen = control_buf.len() as _;
            msghdr.msg_flags = 0;

            let read = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msghdr, 0) };
            if read < 0 {
                let err = io::Error::last_os_error();
                #[cfg(feature = "async")]
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    continue;
                }
                return Err(err.into());
            }

            *last_group = None;
            unsafe {
                let msghdr_ptr = &msghdr as *const libc::msghdr;
                let mut cmsg_ptr: *const libc::cmsghdr = libc::CMSG_FIRSTHDR(msghdr_ptr);
                while !cmsg_ptr.is_null() {
                    let libc::cmsghdr {
                        cmsg_len,
                        cmsg_level,
                        cmsg_type,
                        .. // ignore __pad1 on musl
                    } = *cmsg_ptr;

                    match (cmsg_level, cmsg_type) {
                        (libc::SOL_NETLINK, libc::NETLINK_PKTINFO) => {
                            let data = std::slice::from_raw_parts(
                                libc::CMSG_DATA(cmsg_ptr),
                                cmsg_len as usize - libc::CMSG_LEN(0) as usize,
                            );
                            *last_group = Some(utils::parse_u32(&data[..4]).unwrap());
                        }
                        _ => {}
                    }

                    cmsg_ptr = libc::CMSG_NXTHDR(msghdr_ptr, cmsg_ptr);
                }
            }

            return Ok(read as usize);
        }
    }
}
