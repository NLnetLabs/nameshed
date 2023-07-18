//! The persistent store for data.
//!
//! Currently, all data is kept in memory while the server is running.
//! The persistent storage is only used when the server startes. During
//! operation, all changes are saved to the persistent store.
//!
//! Each entity that needs to store data is given its own area within the
//! store.

use std::{fs, io};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tokio::sync::{Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};


//------------ Store ---------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Store {
    inner: Arc<StoreInner>,
}

#[derive(Debug)]
struct StoreInner {
    /// The base path for all areas.
    areas: PathBuf,
    
    /// The path for the temporary files.
    tmp: PathBuf,

    /// Exclusive access to areas.
    current: Mutex<HashMap<PathBuf, Arc<RwLock<()>>>>,
}

impl Store {
    /// Initializes a store at the given file system location.
    pub fn init(base: PathBuf) -> Result<Self, io::Error> {
        fs::create_dir_all(&base)?;
        let res = Self::open(base)?;
        fs::create_dir_all(&res.inner.areas)?;
        fs::create_dir_all(&res.inner.tmp)?;
        Ok(res)
    }

    /// Opens an existing store at the given file system location.
    pub fn open(base: PathBuf) -> Result<Self, io::Error> {
        Ok(Self {
            inner: Arc::new(StoreInner {
                areas: base.join("areas"),
                tmp: base.join("tmp"),
                current: Default::default(),
            })
        })
    }

    /// Returns the root area.
    pub fn root(&self) -> Area {
        Area::new(self.clone(), self.inner.areas.clone())
    }

    /// Returns the area with the given path.
    pub fn area<'a>(
        &self,
        path: impl IntoIterator<Item = &'a str>
    ) -> Result<Area, io::Error> {
        let res = self.root();
        for item in path {
            res.area(item)?;
        }
        Ok(res)
    }

    async fn read_area(&self, path: PathBuf) -> OwnedRwLockReadGuard<()> {
        self.inner.current.lock().await
            .entry(path).or_default().clone()
            .read_owned().await
    }

    async fn write_area(&self, path: PathBuf) -> OwnedRwLockWriteGuard<()> {
        self.inner.current.lock().await
            .entry(path).or_default().clone()
            .write_owned().await
    }
}


//------------ Area ----------------------------------------------------------

pub struct Area {
    /// The store we are working on.
    store: Store,

    /// The area path of the store.
    path: PathBuf,
}

impl Area {
    fn new(store: Store, path: PathBuf) -> Self {
        Area { store, path }
    }

    pub fn area(&self, name: &str) -> Result<Self, io::Error> {
        Ok(Area::new(
            self.store.clone(), self.path.join("areas").join(name)
        ))
    }

    pub fn area_names(
        &self
    ) -> Result<impl Iterator<Item = String>, io::Error> {
        fs::read_dir(self.path.join("areas")).map(|dir| {
            dir.filter_map(|item| {
                item.ok().and_then(|item| {
                    item.file_name().into_string().ok()
                })
            })
        })
    }

    fn data_path(&self) -> PathBuf {
        self.path.join("data")
    }

    pub async fn read_data(&self) -> Result<ReadData, io::Error> {
        ReadData::new(self).await
    }

    pub async fn append_data(&self) -> Result<AppendData, io::Error> {
        AppendData::new(self).await
    }

    pub async fn replace_data(&self) -> Result<ReplaceData, io::Error> {
        ReplaceData::new(self).await
    }
}



//------------ ReadData ------------------------------------------------------

pub struct ReadData {
    _guard: OwnedRwLockReadGuard<()>,
    file: fs::File,
}

impl ReadData {
    async fn new(area: &Area) -> Result<Self, io::Error> {
        let path = area.data_path();
        let _guard = area.store.read_area(path.clone()).await;
        let file = fs::File::open(path)?;
        Ok(ReadData { _guard, file })
    }

    pub fn deserialize<Ev: for<'de> Deserialize<'de>>(
        &mut self,
    ) -> Result<Ev, io::Error> {
        bincode::deserialize_from(&mut self.file).map_err(map_err)
    }

    pub fn deserialize_opt<Ev: for<'de> Deserialize<'de>>(
        &mut self,
    ) -> Result<Option<Ev>, io::Error> {
        match bincode::deserialize_from(&mut self.file).map_err(map_err) {
            Ok(res) => Ok(Some(res)),
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                Ok(None)
            }
            Err(err) => Err(err)
        }
    }
}

impl io::Read for ReadData {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        self.file.read(buf)
    }
}


//------------ AppendData ----------------------------------------------------

pub struct AppendData {
    _guard: OwnedRwLockWriteGuard<()>,
    file: fs::File,
    delayed_err: Option<io::Error>,
    start: Option<u64>,
}

impl AppendData {
    async fn new(area: &Area) -> Result<Self, io::Error> {
        let path = area.data_path();
        let _guard = area.store.write_area(path.clone()).await;
        let file = fs::OpenOptions::new().write(true).append(true).open(path)?;
        let start = None;
        Ok(AppendData { _guard, file, delayed_err: None, start })
    }

    pub fn serialize(
        &mut self,
        event: impl Serialize
    ) -> Result<(), io::Error> {
        bincode::serialize_into(self, &event).map_err(map_err)
    }

    pub fn serialize_delay_err(
        &mut self,
        event: impl Serialize
    ) {
        if let Err(err) = self.serialize(&event) {
            self.delayed_err = Some(err)
        }
    }

    pub fn commit(&mut self) -> Result<(), io::Error> {
        self.start = None;
        Ok(())
    }
}

impl Drop for AppendData{
    fn drop(&mut self) {
        if let Some(start) = self.start {
           let _ = self.file.set_len(start);
        }
    }
}

impl io::Write for AppendData {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        if let Some(err) = self.delayed_err.take() {
            return Err(err)
        }
        if self.start.is_none() {
            self.start = Some(io::Seek::seek(
                &mut self.file, io::SeekFrom::Current(0)
            )?);
        }
        self.file.write(buf)
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        if let Some(err) = self.delayed_err.take() {
            return Err(err)
        }
        self.file.flush()
    }
}


//------------ ReplaceData ---------------------------------------------------

pub struct ReplaceData {
    _guard: OwnedRwLockWriteGuard<()>,
    path: PathBuf,
    file: NamedTempFile,
}

impl ReplaceData {
    async fn new(area: &Area) -> Result<Self, io::Error> {
        let path = area.data_path();
        let _guard = area.store.write_area(path.clone()).await;
        Ok(ReplaceData {
            path, _guard,
            file: NamedTempFile::new_in(&area.store.inner.tmp)?
        })
    }

    pub fn serialize(
        &mut self,
        event: impl Serialize
    ) -> Result<(), io::Error> {
        bincode::serialize_into(self, &event).map_err(map_err)
    }

    pub fn rollback(self) -> Result<(), io::Error> {
        Ok(())
    }

    pub fn commit(self) -> Result<(), io::Error> {
        match self.file.persist(self.path) {
            Ok(_) => Ok(()),
            Err(err) => Err(err.error)
        }
    }
}

impl io::Write for ReplaceData {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        self.file.flush()
    }
}


//------------ Helper Functions ---------------------------------------------

/// Maps a bincode error into a IO error.
fn map_err(err: bincode::Error) -> io::Error {
    if let bincode::ErrorKind::Io(err) = *err {
        err
    }
    else {
        io::Error::new(io::ErrorKind::Other, err)
    }
}

