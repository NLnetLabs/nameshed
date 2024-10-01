use core::any::Any;
use core::future::Future;
use core::pin::Pin;

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use octseq::Parser;
use tokio::sync::mpsc;
use tracing::{debug, info};

use domain::base::iana::Class;
use domain::base::record::ComposeRecord;
use domain::base::{Name, ParsedName, ParsedRecord, Record};
use domain::rdata::ZoneRecordData;
use domain::zonetree::{
    InMemoryZoneDiff, ReadableZone, Rrset, StoredName, WritableZone, WritableZoneNode, Zone,
    ZoneStore,
};

//------------ ArchiveZone ---------------------------------------------------

// https://datatracker.ietf.org/doc/html/rfc5936#section-6
// 6.  Zone Integrity
//   "An AXFR client MUST ensure that only a successfully transferred copy of
//    the zone data can be used to serve this zone.  Previous description and
//    implementation practice has introduced a two-stage model of the whole
//    zone synchronization procedure:  Upon a trigger event (e.g., when
//    polling of a SOA resource record detects a change in the SOA serial
//    number, or when a DNS NOTIFY request [RFC1996] is received), the AXFR
//    session is initiated, whereby the zone data are saved in a zone file or
//    database (this latter step is necessary anyway to ensure proper restart
//    of the server);"
//
// ArchiveZone demonstrates persisting a zone on commit, e.g. as part of XFR.
// By wrapping a Zone in an ArchiveZone and then using it with the
// ZoneMaintainer the ArchiveZone will see the commit operation performed by
// the ZoneMaintainer as part of XFR processing and can persist the data to
// disk at that point.
//
// One known issue at present is that there is no way for ArchiveZone to see
// the new version of the zone pre-commit, only post-commit. Ideally we would
// verify that the zone has been persisted before allowing the commit to
// continue.
#[derive(Debug)]
pub struct ArchiveZone {
    store: Arc<dyn ZoneStore>,
    write_path: PathBuf,
}

impl ArchiveZone {
    pub fn new(zone: Zone, write_path: &Path) -> Self {
        let write_path = write_path.join(format!("{}.zone", zone.apex_name().to_string()));
        Self {
            store: zone.into_inner(),
            write_path,
        }
    }
}

impl ZoneStore for ArchiveZone {
    fn class(&self) -> Class {
        self.store.class()
    }

    fn apex_name(&self) -> &StoredName {
        self.store.apex_name()
    }

    fn read(self: Arc<Self>) -> Box<dyn ReadableZone> {
        self.store.clone().read()
    }

    fn write(
        self: Arc<Self>,
    ) -> Pin<Box<dyn Future<Output = Box<dyn WritableZone>> + Send + Sync>> {
        let fut = self.store.clone().write();
        Box::pin(async move {
            let writable_zone = fut.await;
            let writable_zone = WritableArchiveZone {
                writable_zone,
                store: self.store.clone(),
                write_path: self.write_path.clone(),
            };
            Box::new(writable_zone) as Box<dyn WritableZone>
        })
    }

    fn as_any(&self) -> &dyn Any {
        self as &dyn Any
    }
}

struct WritableArchiveZone {
    writable_zone: Box<dyn WritableZone>,
    store: Arc<dyn ZoneStore>,
    write_path: PathBuf,
}

impl WritableZone for WritableArchiveZone {
    fn open(
        &self,
        create_diff: bool,
    ) -> Pin<
        Box<dyn Future<Output = Result<Box<dyn WritableZoneNode>, std::io::Error>> + Send + Sync>,
    > {
        self.writable_zone.open(create_diff)
    }

    fn commit(
        &mut self,
        bump_soa_serial: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Option<InMemoryZoneDiff>, std::io::Error>> + Send + Sync>>
    {
        let fut = self.writable_zone.commit(bump_soa_serial);
        let store = self.store.clone();
        let write_path = self.write_path.clone();

        Box::pin(async move {
            debug!("Committing zone");
            let res = fut.await;

            info!("Writing zone to file: {}", write_path.display());
            let mut file = File::create(write_path).unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel::<String>();
            let read = store.read();

            tokio::spawn(async move {
                while let Some(line) = rx.recv().await {
                    writeln!(file, "{}", line).unwrap();
                }
            });

            read.walk(Box::new(move |owner, rrset| {
                dump_rrset(owner, rrset, &tx);
            }));

            info!("Write complete");
            res
        })
    }
}

// Copied from examples/query-zone.rs.
fn dump_rrset(owner: Name<Bytes>, rrset: &Rrset, sender: &mpsc::UnboundedSender<String>) {
    //
    // The following code renders an owner + rrset (IN class, TTL, RDATA)
    // into zone presentation format. This can be used for diagnostic
    // dumping.
    //
    let mut target = Vec::<u8>::new();
    for item in rrset.data() {
        let record = Record::new(owner.clone(), Class::IN, rrset.ttl(), item);
        if record.compose_record(&mut target).is_ok() {
            let mut parser = Parser::from_ref(&target);
            if let Ok(parsed_record) = ParsedRecord::parse(&mut parser) {
                if let Ok(Some(record)) =
                    parsed_record.into_record::<ZoneRecordData<_, ParsedName<_>>>()
                {
                    sender.send(format!("{record}")).unwrap();
                }
            }
        }
    }
}
