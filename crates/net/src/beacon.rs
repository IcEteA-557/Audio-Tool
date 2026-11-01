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
    socket.send_to(payload, SocketAddrV4::new(Ipv4Addr::BROADCAST, BEACON_PORT))?;
    if let Some(mask) = mask {
        let dest = broadcast_v4(ip, mask);
        if dest != Ipv4Addr::BROADCAST && dest != ip {
            let _ = socket.send_to(payload, SocketAddrV4::new(dest, BEACON_PORT));
        }
    }
    Ok(())
}
