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

use std::{marker::PhantomData, path::Path};

//----------- PathMonitor ------------------------------------------------------

/// A monitor for changes to paths.
pub struct PathMonitor<CH: PathChangeHandler> {
    /// The associated handlers.
    _handlers: PhantomData<Vec<CH>>,
}

impl<CH: PathChangeHandler> PathMonitor<CH> {
    /// Set up a [`PathMonitor`].
    pub fn new() -> (Self, PathMonitorRunner<CH>) {
        (
            Self {
                _handlers: PhantomData,
            },
            PathMonitorRunner {
                _public: PhantomData,
            },
        )
    }

    /// Watch a path.
    pub fn add(&self, handler: CH) {
        let _ = handler;
    }
}

//----------- PathMonitorRunner ------------------------------------------------

/// A runner driving a [`PathMonitor`].
pub struct PathMonitorRunner<CH: PathChangeHandler> {
    /// The associated public state.
    _public: PhantomData<PathMonitor<CH>>,
}

impl<CH: PathChangeHandler> PathMonitorRunner<CH> {
    /// Drive the associated [`PathMonitor`], non-blockingly.
    pub fn poll(&mut self) {}
}

//----------- PathChangeHandler ------------------------------------------------

/// A watcher for changes to a path.
pub trait PathChangeHandler {
    /// The path to watch.
    fn path(&self) -> &Path;

    /// React to a change for the path.
    fn react(&self);
}
