use core::net::{IpAddr, SocketAddr};
use core::str::FromStr;

use domain::tsig::KeyName;
use log::error;

use crate::common::tsig::TsigKeyStore;
use crate::log::ExitError;
use crate::zonemaintenance::types::{NotifyConfig, XfrConfig};

pub fn parse_xfr_acl(
    xfr_in: &str,
    xfr_cfg: &mut XfrConfig,
    notify_cfg: &mut NotifyConfig,
    key_store: &TsigKeyStore,
) -> Result<SocketAddr, ExitError> {
    let parts: Vec<String> = xfr_in.split(" KEY ").map(ToString::to_string).collect();
    let (addr, key_name) = match parts.len() {
        1 => (&parts[0], None),
        2 => (&parts[0], Some(&parts[1])),
        _ => {
            error!(
                "Invalid XFR ACL specification: Should be <addr>[:<port>][ KEY <TSIG key name>]"
            );
            return Err(ExitError);
        }
    };
    let addr = SocketAddr::from_str(addr)
        .or(IpAddr::from_str(addr).map(|ip| SocketAddr::new(ip, 0)))
        .inspect_err(|err| error!("Error: Invalid XFR ACL address '{addr}': {err}"))
        .map_err(|_| ExitError)?;

    xfr_cfg.tsig_key = None;
    notify_cfg.tsig_key = None;

    if let Some(key_name) = key_name {
        let encoded_key_name = KeyName::from_str(key_name)
            .inspect_err(|err| error!("Error: Invalid TSIG key name '{key_name}': {err}"))
            .map_err(|_| ExitError)?;
        if let Some(key) = key_store.get_key_by_name(&encoded_key_name) {
            xfr_cfg.tsig_key = Some((encoded_key_name.clone(), key.algorithm()));
            notify_cfg.tsig_key = Some((encoded_key_name, key.algorithm()));
        }
    }

    Ok(addr)
}
