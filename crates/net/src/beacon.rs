use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};

use audiolink_protocol::{Beacon, BEACON_PORT};
use socket2::{Domain, Protocol, Socket, Type};

use crate::nic::broadcast_v4;

pub struct BeaconSocket {
    socket: UdpSocket,
}

impl BeaconSocket {
    pub fn bind() -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, BEACON_PORT).into())?;
        socket.set_broadcast(true)?;
        Ok(Self {
            socket: socket.into(),
        })
    }

    pub fn recv(&self) -> io::Result<(Beacon, Ipv4Addr)> {
        let mut buf = [0u8; 512];
        let (len, addr) = self.socket.recv_from(&mut buf)?;
        let SocketAddr::V4(addr) = addr else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "不是 IPv4 信标"));
        };
        let beacon = Beacon::decode(&buf[..len])
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "无法解析信标"))?;
        Ok((beacon, *addr.ip()))
    }
}

pub fn send_beacon(ip: Ipv4Addr, mask: Option<Ipv4Addr>, payload: &[u8]) -> io::Result<()> {
    let socket = UdpSocket::bind(SocketAddrV4::new(ip, 0))?;
    socket.set_broadcast(true)?;
    let mut delivered = false;
    let mut last_err = None;
    // 有些网卡拒绝 255.255.255.255。失败后仍要试子网广播，否则信标发不出去。
    match socket.send_to(payload, SocketAddrV4::new(Ipv4Addr::BROADCAST, BEACON_PORT)) {
        Ok(_) => delivered = true,
        Err(err) => last_err = Some(err),
    }
    if let Some(mask) = mask {
        let dest = broadcast_v4(ip, mask);
        if dest != Ipv4Addr::BROADCAST && dest != ip {
            match socket.send_to(payload, SocketAddrV4::new(dest, BEACON_PORT)) {
                Ok(_) => delivered = true,
                Err(err) => last_err = Some(err),
            }
        }
    }
    if delivered {
        Ok(())
    } else {
        Err(last_err.unwrap_or_else(|| io::Error::other("没有发出信标")))
    }
}
