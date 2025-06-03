//------------ ListenAddr ----------------------------------------------------

use core::fmt;
use std::{net::SocketAddr, str::FromStr};

use serde_with::DeserializeFromStr;

/// An address and the protocol to serve queries on.
#[derive(Clone, Debug, DeserializeFromStr)]
pub enum ListenAddr {
    /// Plain, unencrypted UDP.
    Udp(SocketAddr),

    /// Plain, unencrypted TCP.
    Tcp(SocketAddr),
}

impl std::fmt::Display for ListenAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListenAddr::Udp(addr) => write!(f, "Udp({})", addr),
            ListenAddr::Tcp(addr) => write!(f, "Tcp({})", addr),
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
            Err(_) => return Err(format!("invalid listen address '{}'", addr)),
        };
        match protocol {
            "udp" => Ok(ListenAddr::Udp(addr)),
            "tcp" => Ok(ListenAddr::Tcp(addr)),
            other => Err(format!("unknown protocol '{}'", other)),
        }
    }
}
