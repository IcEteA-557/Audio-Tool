use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

pub fn listen_tcp(bind_ip: Ipv4Addr, port: u16) -> io::Result<TcpListener> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddrV4::new(bind_ip, port).into())?;
    socket.listen(16)?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

pub fn connect_tcp(bind_ip: Ipv4Addr, peer: SocketAddrV4, timeout: Duration) -> io::Result<TcpStream> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    socket.bind(&SocketAddrV4::new(bind_ip, 0).into())?;
    socket.connect_timeout(&SocketAddr::from(peer).into(), timeout)?;
    let stream: TcpStream = socket.into();
    stream.set_nodelay(true)?;
    stream.set_nonblocking(true)?;
    Ok(stream)
}

pub fn bind_udp(bind_ip: Ipv4Addr) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind(SocketAddrV4::new(bind_ip, 0))?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}
