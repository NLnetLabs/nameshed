use std::fmt::Display;
use std::net::{IpAddr, SocketAddr};

use bytes::Bytes;
use camino::{Utf8Path, Utf8PathBuf};
use domain::base::Name;
use serde::{Deserialize, Serialize};

const DEFAULT_AXFR_PORT: u16 = 53;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ZoneAdd {
    pub name: Name<Bytes>,
    pub source: ZoneSource,
    pub policy: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ZoneAddResult {
    pub name: Name<Bytes>,
    pub status: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ZoneRemoveResult {}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum ZoneSource {
    Zonefile { path: Box<Utf8Path> },
    Server { addr: SocketAddr },
}

impl Display for ZoneSource {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl From<&str> for ZoneSource {
    fn from(s: &str) -> Self {
        if let Ok(addr) = s.parse::<SocketAddr>() {
            ZoneSource::Server { addr }
        } else if let Ok(addr) = s.parse::<IpAddr>() {
            ZoneSource::Server {
                addr: SocketAddr::new(addr, DEFAULT_AXFR_PORT),
            }
        } else {
            ZoneSource::Zonefile {
                path: Utf8PathBuf::from(s).into_boxed_path(),
            }
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ZonesListResult {
    pub zones: Vec<ZoneStatus>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum ZoneStage {
    Unsigned,
    Signed,
    Published,
}

impl Display for ZoneStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            ZoneStage::Unsigned => "unsigned",
            ZoneStage::Signed => "signed",
            ZoneStage::Published => "published",
        };
        f.write_str(str)
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum ZoneStatusError {
    ZoneDoesNotExist,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ZoneStatus {
    pub name: Name<Bytes>,
    pub source: String, // ZoneSource,
    pub policy: String,
    pub stage: ZoneStage,
    pub key_status: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ZoneReloadResult {
    pub name: Name<Bytes>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ServerStatusResult {
    // pub name: Name<Bytes>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PolicyReloadResult {}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PolicyListResult {
    pub policies: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PolicyShowResult {}
