//--- TCP listener traits ----------------------------------------------------
//
// These traits enable us to swap out the real TCP listener for a mock when
// testing.
#![allow(dead_code)]

use core::fmt;
use std::{net::SocketAddr, str::FromStr};

use serde_with::DeserializeFromStr;
use tokio::net::TcpStream;

#[async_trait::async_trait]
pub trait TcpListenerFactory<T> {
    async fn bind(&self, addr: String) -> std::io::Result<T>;
}

#[async_trait::async_trait]
pub trait TcpListener<T> {
    async fn accept(&self) -> std::io::Result<(T, SocketAddr)>;
}

#[async_trait::async_trait]
pub trait TcpStreamWrapper {
    fn into_inner(self) -> std::io::Result<TcpStream>;
}

/// A thin wrapper around the real Tokio TcpListener.
pub struct StandardTcpListenerFactory;

#[async_trait::async_trait]
impl TcpListenerFactory<StandardTcpListener> for StandardTcpListenerFactory {
    async fn bind(&self, addr: String) -> std::io::Result<StandardTcpListener> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        Ok(StandardTcpListener(listener))
    }
}

pub struct StandardTcpListener(::tokio::net::TcpListener);

/// A thin wrapper around the real Tokio TcpListener bind call.
#[async_trait::async_trait]
impl TcpListener<StandardTcpStream> for StandardTcpListener {
    async fn accept(&self) -> std::io::Result<(StandardTcpStream, SocketAddr)> {
        let (stream, addr) = self.0.accept().await?;
        Ok((StandardTcpStream(stream), addr))
    }
}

pub struct StandardTcpStream(::tokio::net::TcpStream);

/// A thin wrapper around the Tokio TcpListener accept() call result.
impl TcpStreamWrapper for StandardTcpStream {
    fn into_inner(self) -> std::io::Result<TcpStream> {
        Ok(self.0)
    }
}

//------------ ListenAddr ----------------------------------------------------

/// An address and the protocol to serve queries on.
#[derive(Debug, DeserializeFromStr)]
pub enum ListenAddr {
    /// Plain, unencrypted UDP.
    Udp(SocketAddr),

    /// Plain, unencrypted TCP.
    Tcp(SocketAddr),

    /// A provided UDP socket.
    UdpSocket(std::net::UdpSocket),

    /// A provided TCP listener.
    TcpListener(std::net::TcpListener),
}

impl std::fmt::Display for ListenAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListenAddr::Udp(addr) => write!(f, "Udp({addr})"),
            ListenAddr::Tcp(addr) => write!(f, "Tcp({addr})"),
            ListenAddr::UdpSocket(_) => write!(f, "UdpSocket"),
            ListenAddr::TcpListener(_) => write!(f, "TcpListener"),
        }
    }
}

impl FromStr for ListenAddr {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (protocol, addr) = match value.split_once(':') {
            Some(stuff) => stuff,
            None => return Err("expected string '<protocol>:<addr>'".into()),
        };
        let addr = match SocketAddr::from_str(addr) {
            Ok(addr) => addr,
            Err(_) => return Err(format!("invalid listen address '{addr}'")),
        };
        match protocol {
            "udp" => Ok(ListenAddr::Udp(addr)),
            "tcp" => Ok(ListenAddr::Tcp(addr)),
            other => Err(format!("unknown protocol '{other}'")),
        }
    }
}
