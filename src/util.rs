//! Miscellaneous utilities for Cascade.

use std::{
    fs,
    io::{self, Write},
};

use camino::Utf8Path;

/// Atomically write a file.
///
/// # Panics
///
/// Panics if 'path' does not have a containing directory.
pub fn write_file(path: &Utf8Path, contents: &[u8]) -> io::Result<()> {
    // Ensure such a path _can_ exist.
    let dir = path
        .parent()
        .expect("'path' must be a file, so it must have a parent");
    fs::create_dir_all(dir)?;

    // Obtain a temporary file in the same directory.
    let mut tmp_file = tempfile::Builder::new().tempfile_in(dir)?;

    // Fill up the temporary file.
    tmp_file.as_file_mut().write_all(contents)?;

    // Replace the target path with the temporary file.
    let _ = tmp_file.persist(path)?;

    Ok(())
}
