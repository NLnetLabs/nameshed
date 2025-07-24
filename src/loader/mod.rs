//! Loading zones.
//!
//! The zone loader is responsible for maintaining up-to-date copies of the DNS
//! zones known to Nameshed.  Every zone has a configured source (e.g. zonefile,
//! DNS server, etc.) that will be monitored for changes.

#![deny(dead_code)]
#![deny(unused_variables)]

mod server;
