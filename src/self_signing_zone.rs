use core::any::Any;
use core::fmt::Debug;
use core::future::Future;
use core::pin::Pin;

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use core::str::FromStr;
use domain::base::iana::Class;
use domain::base::{Name, Record};
use domain::rdata::dnssec::Timestamp;
use domain::sign::keys::keymeta::DnssecSigningKey;
use domain::sign::keys::keypair::{self, GenerateParams};
use domain::sign::keys::signingkey::SigningKey;
use domain::sign::records::{RrsetIter, SortedRecords};
use domain::sign::signing::config::SigningConfig;
use domain::sign::signing::traits::SignableZoneInPlace;
use domain::zonetree::types::StoredRecordData;
use domain::zonetree::{
    InMemoryZoneDiff, ReadableZone, SharedRrset, StoredName, WritableZone, WritableZoneNode, Zone,
    ZoneStore,
};
use octseq::OctetsInto;
use tracing::debug;

//------------ SelfSigningZone -----------------------------------------------

pub struct SelfSigningZone {
    store: Arc<dyn ZoneStore>,
    signing_rrs: Arc<Mutex<SortedRecords<StoredName, StoredRecordData>>>,
}

impl Debug for SelfSigningZone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignToZone").finish()
    }
}

impl SelfSigningZone {
    pub fn new(in_zone: Zone) -> Self {
        Self {
            store: in_zone.into_inner(),
            signing_rrs: Default::default(),
        }
    }
}

impl ZoneStore for SelfSigningZone {
    fn class(&self) -> Class {
        self.store.class()
    }

    fn apex_name(&self) -> &StoredName {
        self.store.apex_name()
    }

    fn read(self: Arc<Self>) -> Box<dyn ReadableZone> {
        let readable_zone = ReadableSignedZone {
            readable_zone: self.store.clone().read(),
            _store: self.store.clone(),
            signing_rrs: self.signing_rrs.clone(),
        };
        Box::new(readable_zone) as Box<dyn ReadableZone>
    }

    fn write(
        self: Arc<Self>,
    ) -> Pin<Box<dyn Future<Output = Box<dyn WritableZone>> + Send + Sync>> {
        let fut = self.store.clone().write();
        Box::pin(async move {
            let writable_zone = fut.await;
            let writable_zone = WritableSignedZone {
                writable_zone,
                store: self.store.clone(),
                signing_rrs: self.signing_rrs.clone(),
            };
            Box::new(writable_zone) as Box<dyn WritableZone>
        })
    }

    fn as_any(&self) -> &dyn Any {
        self as &dyn Any
    }
}

struct WritableSignedZone {
    writable_zone: Box<dyn WritableZone>,
    store: Arc<dyn ZoneStore>,
    signing_rrs: Arc<Mutex<SortedRecords<StoredName, StoredRecordData>>>,
}

impl WritableZone for WritableSignedZone {
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
        let signing_rrs = self.signing_rrs.clone();

        Box::pin(async move {
            debug!("Signing zone");
            let res = fut.await;

            // Sign the zone and store the resulting RRs in signing_rrs.

            // Temporary: Accumulate the zone into a vec as we can only sign
            // over a slice at the moment, not over an iterator yet (nor can
            // we iterate over a zone yet, only walk it ...).

            let read = store.read();

            let passed_records = signing_rrs.clone();

            read.walk(Box::new(move |owner, rrset, _at_zone_cut| {
                for data in rrset.data() {
                    // WARNING: insert() is slow for large zones,
                    // use extend() instead.
                    passed_records
                        .lock()
                        .unwrap()
                        .insert(Record::new(
                            owner.clone(),
                            Class::IN,
                            rrset.ttl(),
                            data.to_owned(),
                        ))
                        .unwrap();
                }
            }));

            // Generate signing key.
            // TODO: This should come from configuration.

            // Generate a new Ed25519 key.
            let params = GenerateParams::Ed25519;
            let (sec_bytes, pub_bytes) = keypair::generate(params).unwrap();

            // Parse the key into Ring or OpenSSL.
            let key_pair = keypair::KeyPair::from_bytes(&sec_bytes, &pub_bytes).unwrap();

            // Associate the key with important metadata.
            let owner: Name<Vec<u8>> = "www.example.org.".parse().unwrap();
            let owner: Name<Bytes> = owner.octets_into();
            let flags = 257; // key signing key
            let key = SigningKey::new(owner, flags, key_pair);

            // Assign signature validity period and operator intent to the keys.
            let key = key.with_validity(
                Timestamp::from_str("20240101000000").unwrap(),
                Timestamp::from_str("20260101000000").unwrap(),
            );
            let keys = [DnssecSigningKey::new_csk(key)];

            // Create a signing configuration.
            let mut signing_config = SigningConfig::default();

            // Then sign the zone in place.
            signing_rrs
                .lock()
                .unwrap()
                .sign_zone(&mut signing_config, &keys)
                .unwrap();

            res
        })
    }
}

struct ReadableSignedZone {
    readable_zone: Box<dyn ReadableZone>,
    _store: Arc<dyn ZoneStore>,
    signing_rrs: Arc<Mutex<SortedRecords<StoredName, StoredRecordData>>>,
}

impl ReadableZone for ReadableSignedZone {
    fn query(
        &self,
        qname: Name<Bytes>,
        qtype: domain::base::Rtype,
    ) -> Result<domain::zonetree::Answer, domain::zonetree::error::OutOfZone> {
        self.readable_zone.query(qname, qtype)
    }

    fn walk(&self, op: domain::zonetree::WalkOp) {
        let sorted_records = self.signing_rrs.lock().unwrap();
        let rrsets: RrsetIter<StoredName, StoredRecordData> = sorted_records.rrsets();
        for rrset in rrsets {
            let mut out_rrset = domain::zonetree::Rrset::new(rrset.rtype(), rrset.ttl());
            for rr in rrset.iter() {
                out_rrset.push_data(rr.data().to_owned());
            }
            (op)(
                rrset.owner().to_owned(),
                &SharedRrset::new(out_rrset),
                false,
            );
        }
        self.readable_zone.walk(op);
    }

    fn is_async(&self) -> bool {
        false
    }
}
