//! Loading zones from DNS servers.

use std::{cmp::Ordering, iter::Peekable, mem, net::SocketAddr, sync::Arc};

use bytes::Bytes;
use domain::{
    base::iana::Rcode,
    net::{
        client::{
            self,
            request::{RequestMessage, RequestMessageMulti, SendRequest, SendRequestMulti},
        },
        xfr::{
            self,
            protocol::{XfrResponseInterpreter, XfrZoneUpdateIterator},
        },
    },
    new::{
        base::{
            build::MessageBuilder,
            name::NameCompressor,
            wire::{AsBytes, ParseBytesZC, ParseError},
            CanonicalRecordData, HeaderFlags, Message, MessageItem, QClass, QType, Question,
            RClass, RType, Record, Serial,
        },
        rdata::RecordData,
    },
    rdata::ZoneRecordData,
    utils::dst::UnsizedCopy,
    zonetree::types::ZoneUpdate,
};
use tokio::net::TcpStream;

use crate::zone::{
    contents::{self, RegularRecord, SoaRecord},
    loader::DnsServerAddr,
    Zone, ZoneContents,
};

use super::{RefreshError, ReloadError};

//----------- refresh() --------------------------------------------------------

/// Refresh a zone from a DNS server.
///
/// See [`super::refresh()`].
pub async fn refresh(
    zone: &Arc<Zone>,
    addr: &DnsServerAddr,
    latest: Option<Arc<contents::Uncompressed>>,
) -> Result<Option<Serial>, RefreshError> {
    // Fetch the zone.
    let (mut compressed, remote) = if let Some(latest) = latest {
        // Fetch the zone relative to the latest local copy.
        match ixfr(zone, addr, &latest.soa).await? {
            Ixfr::UpToDate => {
                // The local copy is up-to-date.
                return Ok(None);
            }

            Ixfr::Compressed(compressed) => {
                // Coalesce the diffs together.
                let mut compressed = compressed.into_iter();
                let initial = compressed.next().unwrap();
                let compressed = compressed.try_fold(initial, |mut whole, sub| {
                    whole.merge_from_next(&sub).map(|()| whole)
                })?;

                // Forward the local copy through the compressed diffs.
                let remote = latest.forward(&compressed)?;

                (Some((compressed, latest)), remote)
            }

            Ixfr::Uncompressed(remote) => {
                // Compress the local copy using the remote copy.
                let compressed = latest.compress(&remote);

                (Some((compressed, latest)), remote)
            }
        }
    } else {
        // Fetch the whole zone.
        let remote = axfr(zone, addr).await?;

        (None, remote)
    };

    // Loop while the zone contents change from under us.
    loop {
        // Load the latest local copy of the zone.
        let mut data = zone.data.lock().unwrap();
        let Some(contents) = &mut data.contents else {
            // Update the zone contents.
            let remote_serial = remote.soa.rdata.serial;
            data.contents = Some(ZoneContents {
                latest: Arc::new(remote),
                previous: Default::default(),
            });

            return Ok(Some(remote_serial));
        };

        // Check whether a compressed copy (of the right version) is available.
        let Some((compressed, _)) = compressed
            .take()
            .filter(|(_, latest)| Arc::ptr_eq(&contents.latest, latest))
        else {
            let latest = contents.latest.clone();
            std::mem::drop(data);

            // Compress the local copy using the remote copy.
            compressed = Some((latest.compress(&remote), latest));

            continue;
        };

        // Update the zone contents.
        let remote_serial = remote.soa.rdata.serial;
        contents.latest = Arc::new(remote);
        contents.previous.push_back(Arc::new(compressed));

        return Ok(Some(remote_serial));
    }
}

//----------- reload() ---------------------------------------------------------

/// Reload a zone from a DNS server.
///
/// See [`super::reload()`].
pub async fn reload(zone: &Arc<Zone>, addr: &DnsServerAddr) -> Result<Option<Serial>, ReloadError> {
    // Load the full remote zone with an AXFR.
    let remote = axfr(zone, addr).await?;
    let remote_serial = remote.soa.rdata.serial;

    // A compressed copy of the local version of the zone.
    let mut compressed_local: Option<(contents::Compressed, Arc<contents::Uncompressed>)> = None;

    // Loop while the zone contents change from under us.
    loop {
        // Load the latest local copy of the zone.
        let mut data = zone.data.lock().unwrap();
        let Some(contents) = &mut data.contents else {
            // There were no previous contents to the zone.
            //
            // Save the freshly loaded version and return.

            data.contents = Some(ZoneContents {
                latest: Arc::new(remote),
                previous: Default::default(),
            });

            return Ok(Some(remote_serial));
        };

        // Compare the local and remote copies.
        let local_serial = contents.latest.soa.rdata.serial;
        match local_serial.partial_cmp(&remote_serial) {
            Some(Ordering::Less) => {
                // We need to compress the local copy.  Check if we
                // have already done so.

                let local = if let Some((local, _)) = compressed_local
                    .take()
                    .filter(|(_, uncompressed)| Arc::ptr_eq(&contents.latest, uncompressed))
                {
                    // Use the cached result.
                    local
                } else {
                    let local = contents.latest.clone();
                    std::mem::drop(data);

                    // Compress the local copy using the remote copy.
                    compressed_local = Some((local.compress(&remote), local));

                    continue;
                };

                // Update the zone contents.
                contents.latest = Arc::new(remote);
                contents.previous.push_back(Arc::new(local));
                return Ok(Some(remote_serial));
            }

            Some(Ordering::Equal) => {
                // Verify the consistency of the two copies.
                let local = contents.latest.clone();
                std::mem::drop(data);
                if local.eq_unsigned(&remote) {
                    return Ok(None);
                } else {
                    return Err(ReloadError::Inconsistent);
                }
            }

            _ => {
                // The remote copy is outdated.
                return Err(ReloadError::OutdatedRemote {
                    local_serial,
                    remote_serial,
                });
            }
        }
    }
}

//----------- ixfr() -----------------------------------------------------------

/// Perform an incremental zone transfer.
///
/// The server is queried for the diff between the version of the zone indicated
/// by the provided SOA record, and the latest version known to the server.  The
/// diff is transformed into a compressed representation of the _local_ version
/// of the zone.  If the local version is identical to the server's version,
/// [`None`] is returned.
pub async fn ixfr(
    zone: &Arc<Zone>,
    addr: &DnsServerAddr,
    local_soa: &SoaRecord,
) -> Result<Ixfr, IxfrError> {
    // Prepare the IXFR query message.
    let mut buffer = [0u8; 1024];
    let mut compressor = NameCompressor::default();
    let mut builder = MessageBuilder::new(
        &mut buffer,
        &mut compressor,
        0u16.into(),
        *HeaderFlags::default().set_qr(false),
    );
    builder
        .push_question(&Question {
            qname: &zone.name,
            // TODO: 'QType::IXFR'.
            qtype: QType { code: 251.into() },
            qclass: QClass::IN,
        })
        .unwrap();
    builder.push_authority(local_soa).unwrap();
    let message = Bytes::copy_from_slice(builder.finish().as_bytes());
    let message =
        domain::base::Message::from_octets(message).expect("'Message' is at least 12 bytes long");

    // If UDP is supported, try it before TCP.
    if let Some(udp_port) = addr.udp_port {
        // Prepare a UDP client.
        let udp_addr: SocketAddr = (addr.ip, udp_port).into();
        let udp_conn = client::protocol::UdpConnect::new(udp_addr);
        let client = client::dgram::Connection::new(udp_conn);

        // Attempt the IXFR.
        let request = RequestMessage::new(message.clone()).unwrap();
        let response = client.send_request(request).get_response().await?;

        // If the server does not support IXFR, fall back to an AXFR.
        if response.header().rcode() == Rcode::NOTIMP {
            // Query the server for its SOA record only.
            let remote_soa = query_soa(zone, addr).await?;

            if local_soa.rdata.serial.partial_cmp(&remote_soa.rdata.serial) != Some(Ordering::Less)
            {
                // The local copy is up-to-date.
                return Ok(Ixfr::UpToDate);
            }

            // Perform a full AXFR.
            return Ok(Ixfr::Uncompressed(axfr(zone, addr).await?));
        }

        // Process the transfer data.
        let mut interpreter = XfrResponseInterpreter::new();
        let mut updates = interpreter.interpret_response(response)?.peekable();

        match updates.peek().unwrap() {
            Ok(ZoneUpdate::DeleteAllRecords) => {
                // This is an AXFR.
                let _ = updates.next().unwrap();
                let mut all = Vec::new();
                let Some(soa) = process_axfr(&mut all, updates)? else {
                    // Fail: UDP-based IXFR returned a partial AXFR.
                    return Err(IxfrError::IncompleteResponse);
                };

                assert!(interpreter.is_finished());
                let all = all.into_boxed_slice();
                let uncompressed = contents::Uncompressed { soa, all };
                return Ok(Ixfr::Uncompressed(uncompressed));
            }

            Ok(ZoneUpdate::BeginBatchDelete(_)) => {
                // This is an IXFR.
                let mut versions = Vec::new();
                let mut this_soa = None;
                let mut next_soa = None;
                let mut all_this = Vec::new();
                let mut all_next = Vec::new();
                process_ixfr(
                    &mut versions,
                    &mut this_soa,
                    &mut next_soa,
                    &mut all_this,
                    &mut all_next,
                    updates,
                )?;

                if interpreter.is_finished() {
                    return Ok(Ixfr::Compressed(versions));
                } else {
                    // Fail: UDP-based IXFR returned a partial IXFR
                    return Err(IxfrError::IncompleteResponse);
                }
            }

            Ok(ZoneUpdate::Finished(record)) => {
                let ZoneRecordData::Soa(soa) = record.data() else {
                    unreachable!("'ZoneUpdate::Finished' must hold a SOA");
                };

                let serial = Serial::from(soa.serial().into_int());
                if serial <= local_soa.rdata.serial {
                    // The local copy is up-to-date.

                    // TODO: Warn if the remote copy appears outdated?
                    return Ok(Ixfr::UpToDate);
                }

                // The transfer may have been too big for UDP; fall back to
                // a TCP-based IXFR.
            }

            _ => unreachable!(),
        }
    }

    // UDP didn't pan out; attempt a TCP-based IXFR.

    // Prepare a TCP client.
    let tcp_addr: SocketAddr = (addr.ip, addr.tcp_port).into();
    let tcp_conn = TcpStream::connect(tcp_addr)
        .await
        .map_err(IxfrError::Connection)?;
    let (client, transport) = client::stream::Connection::<
        RequestMessage<Bytes>,
        RequestMessageMulti<Bytes>,
    >::new(tcp_conn);
    tokio::task::spawn(transport.run());

    // Attempt the IXFR.
    let request = RequestMessageMulti::new(message).unwrap();
    let mut response = SendRequestMulti::send_request(&client, request);
    let mut interpreter = XfrResponseInterpreter::new();

    // Process the first message.
    let initial = response
        .get_response()
        .await?
        .ok_or(IxfrError::IncompleteResponse)?;

    // If the server does not support IXFR, fall back to an AXFR.
    if initial.header().rcode() == Rcode::NOTIMP {
        // Query the server for its SOA record only.
        let remote_soa = query_soa(zone, addr).await?;

        if local_soa.rdata.serial.partial_cmp(&remote_soa.rdata.serial) != Some(Ordering::Less) {
            // The local copy is up-to-date.
            return Ok(Ixfr::UpToDate);
        }

        // Perform a full AXFR.
        return Ok(Ixfr::Uncompressed(axfr(zone, addr).await?));
    }

    let mut updates = interpreter.interpret_response(initial)?.peekable();

    match updates.peek().unwrap() {
        Ok(ZoneUpdate::DeleteAllRecords) => {
            // This is an AXFR.
            let _ = updates.next().unwrap();
            let mut all = Vec::new();

            // Process the response messages.
            let soa = loop {
                if let Some(soa) = process_axfr(&mut all, updates)? {
                    break soa;
                } else {
                    // Retrieve the next message.
                    let message = response
                        .get_response()
                        .await?
                        .ok_or(IxfrError::IncompleteResponse)?;
                    updates = interpreter.interpret_response(message)?.peekable();
                }
            };

            assert!(interpreter.is_finished());
            let all = all.into_boxed_slice();
            let uncompressed = contents::Uncompressed { soa, all };
            Ok(Ixfr::Uncompressed(uncompressed))
        }

        Ok(ZoneUpdate::BeginBatchDelete(_)) => {
            // This is an IXFR.
            let mut versions = Vec::new();
            let mut this_soa = None;
            let mut next_soa = None;
            let mut all_this = Vec::new();
            let mut all_next = Vec::new();

            // Process the response messages.
            loop {
                process_ixfr(
                    &mut versions,
                    &mut this_soa,
                    &mut next_soa,
                    &mut all_this,
                    &mut all_next,
                    updates,
                )?;

                if interpreter.is_finished() {
                    break;
                } else {
                    // Retrieve the next message.
                    let message = response
                        .get_response()
                        .await?
                        .ok_or(IxfrError::IncompleteResponse)?;
                    updates = interpreter.interpret_response(message)?.peekable();
                }
            }

            assert!(interpreter.is_finished());
            Ok(Ixfr::Compressed(versions))
        }

        Ok(ZoneUpdate::Finished(record)) => {
            let ZoneRecordData::Soa(soa) = record.data() else {
                unreachable!("'ZoneUpdate::Finished' must hold a SOA");
            };

            let serial = Serial::from(soa.serial().into_int());
            if serial <= local_soa.rdata.serial {
                // The local copy is up-to-date.

                // TODO: Warn if the remote copy appears outdated?
                Ok(Ixfr::UpToDate)
            } else {
                Err(IxfrError::InconsistentUpToDate)
            }
        }

        _ => unreachable!(),
    }
}

/// The output from [`ixfr()`].
pub enum Ixfr {
    /// The local copy is up-to-date.
    ///
    /// The local copy's SOA version matches that of the remote copy.
    /// This _should_ mean that the two copies will have the same contents.
    UpToDate,

    /// A sequence of compressed diffs.
    ///
    /// The local copy was outdated with respect to the remote copy.  The
    /// history of zone changes between the two is returned as a sequence of
    /// compressed zone versions, from earliest (the local copy) to newest (the
    /// remote copy).
    Compressed(Vec<contents::Compressed>),

    /// An uncompressed representation of a new zone.
    ///
    /// The local copy was outdated with respect to the remote copy.  The remote
    /// copy has been transferred in its entirety, in an uncompressed state.  A
    /// compressed representation of the local copy can be built from this.
    Uncompressed(contents::Uncompressed),
}

/// Process an IXFR message.
fn process_ixfr(
    versions: &mut Vec<contents::Compressed>,
    this_soa: &mut Option<SoaRecord>,
    next_soa: &mut Option<SoaRecord>,
    only_this: &mut Vec<RegularRecord>,
    only_next: &mut Vec<RegularRecord>,
    updates: Peekable<XfrZoneUpdateIterator<'_, '_>>,
) -> Result<(), IxfrError> {
    for update in updates {
        match update? {
            ZoneUpdate::BeginBatchDelete(record) => {
                // If there was a previous zone version, write it out.
                assert!(this_soa.is_some() == next_soa.is_some());
                if let Some((soa, next_soa)) = this_soa.take().zip(next_soa.take()) {
                    // Sort the contents of the batch addition.
                    only_next.sort_unstable();

                    let only_this = mem::take(only_this).into_boxed_slice();
                    let only_next = mem::take(only_next).into_boxed_slice();
                    versions.push(contents::Compressed {
                        soa,
                        next_soa,
                        only_this,
                        only_next,
                    });
                }

                assert!(this_soa.is_none());
                assert!(next_soa.is_none());
                assert!(only_this.is_empty());
                assert!(only_next.is_empty());

                *this_soa = Some(record.into());
            }

            ZoneUpdate::DeleteRecord(record) => {
                assert!(this_soa.is_some());
                assert!(next_soa.is_none());

                only_this.push(record.into());
            }

            ZoneUpdate::BeginBatchAdd(record) => {
                assert!(this_soa.is_some());
                assert!(next_soa.is_none());
                assert!(only_next.is_empty());

                // Sort the contents of the batch deletion.
                only_this.sort_unstable_by(|l, r| {
                    (&l.rname, l.rtype)
                        .cmp(&(&r.rname, r.rtype))
                        .then_with(|| l.rdata.cmp_canonical(&r.rdata))
                });

                *next_soa = Some(record.into());
            }

            ZoneUpdate::AddRecord(record) => {
                assert!(this_soa.is_some());
                assert!(next_soa.is_some());

                only_next.push(record.into());
            }

            ZoneUpdate::Finished(record) => {
                assert!(this_soa.is_some());
                assert!(next_soa.is_some());

                assert!(*next_soa == Some(record.into()));

                // Sort the contents of the batch addition.
                only_next.sort_unstable();

                let only_this = mem::take(only_this).into_boxed_slice();
                let only_next = mem::take(only_next).into_boxed_slice();
                versions.push(contents::Compressed {
                    soa: this_soa.take().unwrap(),
                    next_soa: next_soa.take().unwrap(),
                    only_this,
                    only_next,
                });

                break;
            }

            _ => unreachable!(),
        }
    }

    Ok(())
}

//----------- axfr() -----------------------------------------------------------

/// Perform an authoritative zone transfer.
pub async fn axfr(
    zone: &Arc<Zone>,
    addr: &DnsServerAddr,
) -> Result<contents::Uncompressed, AxfrError> {
    // Prepare the IXFR query message.
    let mut buffer = [0u8; 512];
    let mut compressor = NameCompressor::default();
    let mut builder = MessageBuilder::new(
        &mut buffer,
        &mut compressor,
        0u16.into(),
        *HeaderFlags::default().set_qr(false),
    );
    builder
        .push_question(&Question {
            qname: &zone.name,
            // TODO: 'QType::AXFR'.
            qtype: QType { code: 252.into() },
            qclass: QClass::IN,
        })
        .unwrap();
    let message = Bytes::copy_from_slice(builder.finish().as_bytes());
    let message =
        domain::base::Message::from_octets(message).expect("'Message' is at least 12 bytes long");

    // Prepare a TCP client.
    let tcp_addr: SocketAddr = (addr.ip, addr.tcp_port).into();
    let tcp_conn = TcpStream::connect(tcp_addr)
        .await
        .map_err(AxfrError::Connection)?;
    let (client, transport) = client::stream::Connection::<
        RequestMessage<Bytes>,
        RequestMessageMulti<Bytes>,
    >::new(tcp_conn);
    tokio::task::spawn(transport.run());

    // Attempt the AXFR.
    let request = RequestMessageMulti::new(message).unwrap();
    let mut response = SendRequestMulti::send_request(&client, request);
    let mut interpreter = XfrResponseInterpreter::new();

    // Process the first message.
    let initial = response
        .get_response()
        .await?
        .ok_or(AxfrError::IncompleteResponse)?;
    let mut updates = interpreter.interpret_response(initial)?.peekable();

    assert!(updates.next().unwrap()? == ZoneUpdate::DeleteAllRecords);
    let mut all = Vec::new();

    // Process the response messages.
    let soa = loop {
        if let Some(soa) = process_axfr(&mut all, updates)? {
            break soa;
        } else {
            // Retrieve the next message.
            let message = response
                .get_response()
                .await?
                .ok_or(AxfrError::IncompleteResponse)?;
            updates = interpreter.interpret_response(message)?.peekable();
        }
    };

    assert!(interpreter.is_finished());
    let all = all.into_boxed_slice();
    Ok(contents::Uncompressed { soa, all })
}

/// Process an AXFR message.
fn process_axfr(
    all: &mut Vec<RegularRecord>,
    updates: Peekable<XfrZoneUpdateIterator<'_, '_>>,
) -> Result<Option<SoaRecord>, AxfrError> {
    // Process the updates.
    for update in updates {
        match update? {
            ZoneUpdate::AddRecord(record) => {
                all.push(record.into());
            }

            ZoneUpdate::Finished(record) => {
                // Sort the zone contents.
                all.sort_unstable_by(|l, r| {
                    (&l.rname, l.rtype)
                        .cmp(&(&r.rname, r.rtype))
                        .then_with(|| l.rdata.cmp_canonical(&r.rdata))
                });

                return Ok(Some(record.into()));
            }

            _ => unreachable!(),
        }
    }

    Ok(None)
}

//----------- query_soa() ------------------------------------------------------

/// Query a DNS server for the SOA record of a zone.
pub async fn query_soa(zone: &Arc<Zone>, addr: &DnsServerAddr) -> Result<SoaRecord, QuerySoaError> {
    // Prepare the SOA query message.
    let mut buffer = [0u8; 512];
    let mut compressor = NameCompressor::default();
    let mut builder = MessageBuilder::new(
        &mut buffer,
        &mut compressor,
        0u16.into(),
        *HeaderFlags::default().set_qr(false),
    );
    builder
        .push_question(&Question {
            qname: &zone.name,
            qtype: QType::SOA,
            qclass: QClass::IN,
        })
        .unwrap();
    let message = Bytes::copy_from_slice(builder.finish().as_bytes());
    let message =
        domain::base::Message::from_octets(message).expect("'Message' is at least 12 bytes long");

    let tcp_addr: SocketAddr = (addr.ip, addr.tcp_port).into();

    // Send the query.
    let response = if let Some(udp_port) = addr.udp_port {
        // Prepare a UDP+TCP client.
        let udp_addr: SocketAddr = (addr.ip, udp_port).into();
        let udp_conn = client::protocol::UdpConnect::new(udp_addr);
        let tcp_conn = client::protocol::TcpConnect::new(tcp_addr);
        let (client, transport) = client::dgram_stream::Connection::new(udp_conn, tcp_conn);
        tokio::task::spawn(transport.run());

        // Send the query.
        let request = RequestMessage::new(message.clone()).unwrap();
        client.send_request(request).get_response().await?
    } else {
        // Prepare a TCP client.
        let tcp_conn = TcpStream::connect(tcp_addr)
            .await
            .map_err(QuerySoaError::Connection)?;
        let (client, transport) = client::stream::Connection::<
            RequestMessage<Bytes>,
            RequestMessageMulti<Bytes>,
        >::new(tcp_conn);
        tokio::task::spawn(transport.run());

        // Send the query.
        let request = RequestMessage::new(message.clone()).unwrap();
        SendRequest::send_request(&client, request)
            .get_response()
            .await?
    };

    // Parse the response message.
    let response = Message::parse_bytes_by_ref(response.as_slice())
        .expect("'Message' is at least 12 bytes long");
    if response.header.flags.rcode() != 0 {
        return Err(QuerySoaError::MismatchedResponse);
    }
    let mut parser = response.parse();
    let Some(MessageItem::Question(Question {
        qname,
        qtype: QType::SOA,
        qclass: QClass::IN,
    })) = parser.next().transpose()?
    else {
        return Err(QuerySoaError::MismatchedResponse);
    };
    if *qname != *zone.name {
        return Err(QuerySoaError::MismatchedResponse);
    }
    let Some(MessageItem::Answer(Record {
        rname,
        rtype: rtype @ RType::SOA,
        rclass: rclass @ RClass::IN,
        ttl,
        rdata: RecordData::Soa(rdata),
    })) = parser.next().transpose()?
    else {
        return Err(QuerySoaError::MismatchedResponse);
    };
    if *rname != *zone.name {
        return Err(QuerySoaError::MismatchedResponse);
    }
    let None = parser.next() else {
        return Err(QuerySoaError::MismatchedResponse);
    };

    Ok(SoaRecord(Record {
        rname: zone.name.unsized_copy_into(),
        rtype,
        rclass,
        ttl,
        rdata: rdata.map_names(|n| n.unsized_copy_into()),
    }))
}

//============ Errors ==========================================================

//----------- IxfrError --------------------------------------------------------

/// An error when performing an incremental zone transfer.
//
// TODO: Expand into less opaque variants.
pub enum IxfrError {
    /// A DNS client error occurred.
    Client(client::request::Error),

    /// Could not connect to the server.
    Connection(std::io::Error),

    /// An XFR interpretation error occurred.
    Xfr(xfr::protocol::Error),

    /// An XFR interpretation error occurred.
    XfrIter(xfr::protocol::IterationError),

    /// An incomplete response was received.
    IncompleteResponse,

    /// An inconsistent [`Ixfr::UpToDate`] response was received.
    InconsistentUpToDate,

    /// A query for a SOA record failed.
    QuerySoa(QuerySoaError),

    /// An AXFR related error occurred.
    Axfr(AxfrError),
}

impl From<client::request::Error> for IxfrError {
    fn from(value: client::request::Error) -> Self {
        Self::Client(value)
    }
}

impl From<xfr::protocol::Error> for IxfrError {
    fn from(value: xfr::protocol::Error) -> Self {
        Self::Xfr(value)
    }
}

impl From<xfr::protocol::IterationError> for IxfrError {
    fn from(value: xfr::protocol::IterationError) -> Self {
        Self::XfrIter(value)
    }
}

impl From<QuerySoaError> for IxfrError {
    fn from(v: QuerySoaError) -> Self {
        Self::QuerySoa(v)
    }
}

impl From<AxfrError> for IxfrError {
    fn from(value: AxfrError) -> Self {
        Self::Axfr(value)
    }
}

//----------- AxfrError --------------------------------------------------------

/// An error when performing an authoritative zone transfer.
pub enum AxfrError {
    /// A DNS client error occurred.
    Client(client::request::Error),

    /// Could not connect to the server.
    Connection(std::io::Error),

    /// An XFR interpretation error occurred.
    Xfr(xfr::protocol::Error),

    /// An XFR interpretation error occurred.
    XfrIter(xfr::protocol::IterationError),

    /// An incomplete response was received.
    IncompleteResponse,
}

impl From<client::request::Error> for AxfrError {
    fn from(value: client::request::Error) -> Self {
        Self::Client(value)
    }
}

impl From<xfr::protocol::Error> for AxfrError {
    fn from(value: xfr::protocol::Error) -> Self {
        Self::Xfr(value)
    }
}

impl From<xfr::protocol::IterationError> for AxfrError {
    fn from(value: xfr::protocol::IterationError) -> Self {
        Self::XfrIter(value)
    }
}

//----------- QuerySoaError ----------------------------------------------------

/// An error when querying a DNS server for a SOA record.
pub enum QuerySoaError {
    /// A DNS client error occurred.
    Client(client::request::Error),

    /// Could not connect to the server.
    Connection(std::io::Error),

    /// The response could not be parsed.
    Parse(ParseError),

    /// The response did not match the query.
    MismatchedResponse,
}

impl From<client::request::Error> for QuerySoaError {
    fn from(v: client::request::Error) -> Self {
        Self::Client(v)
    }
}

impl From<ParseError> for QuerySoaError {
    fn from(v: ParseError) -> Self {
        Self::Parse(v)
    }
}
