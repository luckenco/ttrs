//! Explicit native backup/import/restore capabilities, separate from tracking.
//!
//! SQLite snapshots use online backup, verification, file fsync and atomic
//! no-clobber publication. Portable export reads an independent captured copy.
//! New native handles hold shared maintenance leases; restore requires all of
//! them to be closed, saves a final safety copy and rotates the incarnation.
//! Never replace a live database manually, or use raw SQLite handles during
//! maintenance: old programs do not understand maintenance leases.
//!
//! Publication is followed by directory fsync. An I/O error at that last step
//! can mean the output is already installed; inspect it before retrying.
//!
//! ```
//! use tt::{Timestamp, Tracker, maintenance, portable};
//! let directory = tempfile::tempdir()?;
//! let database = directory.path().join("tt.sqlite");
//! let imported = directory.path().join("imported.sqlite");
//! {
//!     let mut tracker = Tracker::open(&database)?;
//!     tracker.add_client("Acme".into())?;
//! }
//! let snapshot = maintenance::export(&database, "2026-01-02T00:00:00Z".parse::<Timestamp>()?)?;
//! let verified = portable::verify(&snapshot.to_bytes()?)?;
//! maintenance::import(&imported, &verified)?;
//! assert_eq!(Tracker::open(&imported)?.clients()?[0].name(), "Acme");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
use std::{fs::File, io::Write, path::Path};

use crate::{Error, Result, Timestamp, native, portable};

/// Captures a verified standalone SQLite file. Existing outputs are never overwritten.
/// The source must exist; supported legacy sources migrate with a safety copy.
pub fn backup(database: impl AsRef<Path>, output: impl AsRef<Path>) -> Result<()> {
    native::Store::open_existing(database.as_ref())?.backup(output.as_ref())
}

/// Captures every domain row and sequence from one independent SQLite snapshot.
/// `captured_at` is host-supplied, UTC and normalized to microseconds.
/// The source must exist; supported legacy sources migrate with a safety copy.
pub fn export(database: impl AsRef<Path>, captured_at: Timestamp) -> Result<portable::Verified> {
    let snapshot = native::Store::open_existing(database.as_ref())?.capture()?;
    portable::capture(&native::Adapter(&snapshot), captured_at)
}

/// Publishes a portable file atomically after fsync, without overwriting an output.
/// The snapshot has already passed verification; files contain tracking data, not secrets.
pub fn save(snapshot: &portable::Verified, output: impl AsRef<Path>) -> Result<()> {
    let output = output.as_ref();
    let directory = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(directory).map_err(storage)?;
    temp.write_all(&snapshot.to_bytes()?).map_err(storage)?;
    temp.as_file().sync_all().map_err(storage)?;
    temp.persist_noclobber(output)
        .map_err(|error| storage(error.error))?;
    File::open(directory)
        .map_err(storage)?
        .sync_all()
        .map_err(storage)?;
    Ok(())
}

/// Imports into a nonexistent target only. Identity is new; numeric IDs, running
/// timers, revision and deleted-ID high-water marks are preserved exactly.
pub fn import(database: impl AsRef<Path>, snapshot: &portable::Verified) -> Result<()> {
    native::Store::import(database.as_ref(), |tx| portable::import(tx, snapshot))
}

/// Explicitly replaces a closed workspace with verified historical data.
///
/// **Destructive:** calling this function confirms discarding work absent from
/// `snapshot`. The old data is retained in a verified, unique `pre-restore` SQLite
/// safety copy. Workspace identity is preserved, incarnation is new, running
/// timers remain running, and imported receipts cannot authorize old requests.
/// This is the low-level native restore drill; reviewed loss reports/activation
/// and authority-checked application operations belong to later service stages.
pub fn restore(database: impl AsRef<Path>, snapshot: &portable::Verified) -> Result<()> {
    native::Store::restore(database.as_ref(), |tx| portable::import(tx, snapshot))
}

fn storage(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Storage(Box::new(error))
}
