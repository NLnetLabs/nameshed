use core::net::{IpAddr, SocketAddr};
use core::str::FromStr;

use domain::tsig::KeyName;

use crate::common::tsig::TsigKeyStore;
use crate::zonemaintenance::types::{NotifyConfig, XfrConfig};

/// Parse a given ACL string of the form "<addr>[:<port>][ KEY <TSIG key name>]"
/// filling in the given [`XfrConfig`] and [`NotifyConfig`] objects with TSIG
/// details and returning the determined SocketAddr (which will have port 0 if
/// no port was specified).
///
/// A returned SocketAddr without port permits XFR only, while with a port
/// it also permits NOTIFY. The direction (XFR in/out, NOTIFY in/out) is
/// dependent on where the ACL is used.
pub fn parse_xfr_acl(
    acl: &str,
    xfr_cfg: &mut XfrConfig,
    notify_cfg: &mut NotifyConfig,
    key_store: &TsigKeyStore,
) -> Result<SocketAddr, String> {
    let parts: Vec<String> = acl.split(" KEY ").map(ToString::to_string).collect();
    let (addr, key_name) = match parts.len() {
        1 => (&parts[0], None),
        2 => (&parts[0], Some(&parts[1])),
        _ => {
            return Err(format!("Invalid XFR ACL specification '{acl}': Should be <addr>[:<port>][ KEY <TSIG key name>]"));
        }
    };
    let addr = SocketAddr::from_str(addr)
        .or(IpAddr::from_str(addr).map(|ip| SocketAddr::new(ip, 0)))
        .map_err(|err| format!("Error: Invalid XFR ACL address '{addr}': {err}"))?;

    xfr_cfg.tsig_key = None;
    notify_cfg.tsig_key = None;

    if let Some(key_name) = key_name {
        let encoded_key_name = KeyName::from_str(key_name)
            .map_err(|err| format!("Error: Invalid TSIG key name '{key_name}': {err}"))?;
        if let Some(key) = key_store.get_key_by_name(&encoded_key_name) {
            xfr_cfg.tsig_key = Some((encoded_key_name.clone(), key.algorithm()));
            notify_cfg.tsig_key = Some((encoded_key_name, key.algorithm()));
        }
    }

    Ok(addr)
}
