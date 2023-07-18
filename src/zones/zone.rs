
use std::io;
use std::sync::{Arc, Weak};
use domain::base::iana::Class;
use tokio::sync::{Mutex, RwLock};
use crate::store::Store;
use super::flavor::Flavor;
use super::nodes::ZoneApex;
use super::read::ReadZone;
use super::rrset::StoredDname;
use super::versioned::Version;
use super::write::WriteZone;


//------------ Zone ----------------------------------------------------------

pub struct Zone {
    apex: Arc<ZoneApex>,
    versions: Arc<RwLock<ZoneVersions>>,
    update_lock: Arc<Mutex<()>>,
}

impl Zone {
    pub(super) fn new(apex: Arc<ZoneApex>) -> Self {
        Zone {
            apex,
            versions: Default::default(),
            update_lock: Default::default(),
        }
    }

    pub(super) async fn load(
        store: &Store, apex_name: StoredDname, class: Class
    ) -> Result<Self, io::Error> {
        let mut apex = ZoneApex::new(apex_name, class);
        let mut store = stored::area(store, &apex)?.read_data().await?;
        apex.load_snapshot(&mut store)?;
        let zone = Self::new(apex.into());
        WriteZone::new(
            zone.apex.clone(),
            zone.update_lock.clone().lock_owned().await,
            zone.versions.read().await.current.0.next(),
            zone.versions.clone(),
            None
        ).load(&mut store)?;
        Ok(zone)
    }

    pub(super) fn apex(&self) -> &ZoneApex {
        &self.apex
    }

    pub fn class(&self) -> Class {
        self.apex.class()
    }

    pub fn apex_name(&self) -> &StoredDname {
        self.apex.apex_name()
    }

    pub fn read(&self, flavor: Option<Flavor>) -> ReadZone {
        let (version, marker) = self.versions.blocking_read().current.clone();
        ReadZone::new(self.apex.clone(), flavor, version, marker)
    }

    pub async fn write(&self, store: &Store) -> Result<WriteZone, io::Error> {
        Ok(WriteZone::new(
            self.apex.clone(),
            self.update_lock.clone().lock_owned().await,
            self.versions.read().await.current.0.next(),
            self.versions.clone(),
            Some(stored::area(store, &self.apex)?.append_data().await?),
        ))
    }

    pub async fn snapshot(&self, store: &Store) -> Result<(), io::Error> {
        let _ = self.update_lock.clone().lock_owned().await;
        let (version, _) = self.versions.read().await.current.clone();
        let mut store = stored::area(store, &self.apex)?.replace_data().await?;
        self.apex.snapshot(&mut store, version)?;
        store.commit()
    }
}


//------------ ZoneVersions --------------------------------------------------

pub(super) struct ZoneVersions {
    current: (Version, Arc<VersionMarker>),
    all: Vec<(Version, Weak<VersionMarker>)>,
}

impl ZoneVersions {
    pub fn update_current(&mut self, version: Version) -> Arc<VersionMarker> {
        let marker = Arc::new(VersionMarker);
        self.current = (version, marker.clone());
        marker
    }

    pub fn push_version(
        &mut self, version: Version, marker: Arc<VersionMarker>
    ) {
        self.all.push((version, Arc::downgrade(&marker)))
    }

    pub fn clean_versions(&mut self) -> Option<Version> {
        let mut max_version = None;
        self.all.retain(|item| {
            if item.1.strong_count() > 0 {
                true
            }
            else {
                match max_version {
                    Some(old) => {
                        if item.0 > old {
                            max_version = Some(item.0)
                        }
                    }
                    None => max_version = Some(item.0)
                }
                false
            }
        });
        max_version
    }
}

impl Default for ZoneVersions {
    fn default() -> Self {
        let marker = Arc::new(VersionMarker);
        let weak_marker = Arc::downgrade(&marker);
        ZoneVersions {
            current: (Version::default(), marker),
            all: vec![(Version::default(), weak_marker)]
        }
    }
}


//------------ VersionMarker -------------------------------------------------

pub(super) struct VersionMarker;


//============ Stored Data ===================================================

mod stored {
    use std::io;
    use crate::store::{Area, Store};
    use super::ZoneApex;

    pub fn area(store: &Store, apex: &ZoneApex) -> Result<Area, io::Error> {
        store.area([
            "zones",
            apex.display_class().as_ref(),
            apex.apex_name_display()
        ])
    }
}

