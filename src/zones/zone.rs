
use std::sync::{Arc, Weak};
use parking_lot::RwLock;
use tokio::sync::Mutex as AsyncMutex;
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
    update_lock: Arc<AsyncMutex<()>>,
}

impl Zone {
    pub(super) fn new(apex: Arc<ZoneApex>) -> Self {
        Zone {
            apex,
            versions: Default::default(),
            update_lock: Default::default(),
        }
    }

    pub fn apex_name(&self) -> &StoredDname {
        self.apex.apex_name()
    }

    pub fn read(&self, flavor: Option<Flavor>) -> ReadZone {
        let (version, marker) = self.versions.read().current.clone();
        ReadZone::new(self.apex.clone(), flavor, version, marker)
    }

    pub async fn write(&self) -> WriteZone {
        WriteZone::new(
            self.apex.clone(),
            self.update_lock.clone().lock_owned().await,
            self.versions.read().current.0.next(),
            self.versions.clone(),
        )
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

