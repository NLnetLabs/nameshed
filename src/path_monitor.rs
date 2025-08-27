//! Monitoring filesystem paths.
//!
//! This module provides an efficient, bullet-proof mechanism for watching
//! paths in the filesystem for changes.  It guarantees that changes to any
//! watched path will result in the associated event hooks being activated.
//!
//! ## Linux
//!
//! NOTE: The implementation is a work in progress and does not capture all of
//! the documented change conditions below.
//!
//! On Linux, a path is considered to have changed if one or more of the
//! following (exhaustive) conditions occur:
//!
//! - The path corresponds to (a symlink to) a regular file, which some process
//!   opened for writing, and the process closes the file.
//!
//! - The path corresponds to (a symlink to) an object which could not be
//!   accessed due to a permissions error, and it becomes accessible.
//!
//! - The path corresponds to (a symlink to) an accessible object, and it
//!   becomes inaccessible.
//!
//! - The path corresponds to (a symlink to) an object that did not previously
//!   exist, and it is created.
//!
//! - The path corresponds to (a symlink to) an existing object, and it is
//!   deleted or moved away.
//!
//! - The path corresponds to (a symlink to) an object, and it is replaced by
//!   another object due to an atomic on-filesystem move.
//!
//! - A directory along the path (or the path it points to, if it is a symlink)
//!   is mounted or unmounted.

use std::{
    collections::hash_map,
    path::Path,
    sync::{Arc, Mutex, Weak},
};

use notify::{RecursiveMode, Watcher};

//----------- PathMonitor ------------------------------------------------------

/// A monitor for changes to paths.
pub struct PathMonitor<CH: PathChangeHandler> {
    /// The underlying monitor.
    inner: Arc<Mutex<Inner<CH>>>,
}

struct Inner<CH> {
    /// The underlying watcher.
    watcher: notify::RecommendedWatcher,

    /// A map of all paths to handlers.
    handlers: foldhash::HashMap<Box<Path>, Vec<Arc<CH>>>,
}

impl<CH: PathChangeHandler> PathMonitor<CH> {
    /// Set up a [`PathMonitor`].
    #[allow(clippy::new_without_default)] // will return Result later
    pub fn new() -> Self {
        let inner = Arc::new_cyclic(|inner: &Weak<Mutex<Inner<CH>>>| {
            let handler_inner = inner.clone();
            let handler = move |event: notify::Result<notify::Event>| {
                let Some(inner) = handler_inner.upgrade() else {
                    return;
                };

                let Ok(lock) = inner.lock() else {
                    return;
                };

                let paths = match event {
                    Ok(event) => event.paths,
                    Err(error) => error.paths,
                };

                for path in paths {
                    let Some(handlers) = lock.handlers.get(&*path) else {
                        continue;
                    };

                    for handler in handlers {
                        handler.react();
                    }
                }
            };

            let watcher = notify::recommended_watcher(handler).unwrap();
            let handlers = Default::default();
            Mutex::new(Inner { watcher, handlers })
        });

        Self {
            inner: inner.clone(),
        }
    }

    /// Watch a path.
    pub fn add(&self, handler: CH) -> Arc<CH> {
        let handler = Arc::new(handler);
        let path = handler.path();
        let mut lock = self.inner.lock().unwrap();
        let inner = &mut *lock;

        let handlers = inner.handlers.entry(path.into()).or_default();
        if handlers.is_empty() {
            inner
                .watcher
                .watch(path, RecursiveMode::NonRecursive)
                .unwrap();
        }

        handlers.push(handler.clone());

        handler
    }

    /// Stop watching a path.
    pub fn remove(&self, handler: Arc<CH>) {
        let path = handler.path();
        let mut lock = self.inner.lock().unwrap();

        let mut handlers = match lock.handlers.entry(path.into()) {
            hash_map::Entry::Occupied(entry) => entry,
            hash_map::Entry::Vacant(_) => panic!("handler not registered"),
        };

        // Remove this handler from the list.
        handlers.get_mut().retain(|h| !Arc::ptr_eq(h, &handler));

        if handlers.get_mut().is_empty() {
            handlers.remove();
            let _ = lock.watcher.unwatch(path);
        }
    }
}

//----------- PathChangeHandler ------------------------------------------------

/// A watcher for changes to a path.
pub trait PathChangeHandler: Send + Sync + 'static {
    /// The path to watch.
    fn path(&self) -> &Path;

    /// React to a change for the path.
    fn react(&self);
}
