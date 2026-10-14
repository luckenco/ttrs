//! Per-request durable recovery. Installation is no-clobber and never holds a lock over HTTP.
//! Files contain original endpoint/context and exact envelope bytes, never credentials.
//! Requires a local filesystem supporting hard links and file/directory fsync.
//! No installation identity is stored: request IDs use independent 128-bit UUID-v4 nonces.
//! Journals need no additional persistent identifier (or cross-installation tracking state).
use crate::protocol;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const LIMIT: u64 = 4 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub endpoint: String,
    pub envelope: Vec<u8>,
    #[serde(default, deserialize_with = "deserialize_outcome")]
    pub outcome: Option<JournalOutcome>,
}
/// Durable terminal knowledge. `NotApplied` requires a serialized server status read.
/// Legacy unwrapped recorded outcomes remain readable without changing envelope bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JournalOutcome {
    Recorded(protocol::RecordedOutcome),
    NotApplied,
}
fn deserialize_outcome<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<JournalOutcome>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Saved {
        Current(JournalOutcome),
        Legacy(protocol::RecordedOutcome),
    }
    Ok(
        Option::<Saved>::deserialize(deserializer)?.map(|saved| match saved {
            Saved::Current(outcome) => outcome,
            Saved::Legacy(outcome) => JournalOutcome::Recorded(outcome),
        }),
    )
}
impl Pending {
    pub fn request(&self) -> crate::Result<protocol::Envelope> {
        protocol::Envelope::from_bytes(&self.envelope)
    }
}
// Each newly created directory entry must be durable in its own parent, not just
// the leaf's parent. Errors propagate to the caller before any network admission.
fn create_dir(dir: &Path, sync_parent: &impl Fn(&Path) -> io::Result<()>) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    create_dir(parent, sync_parent)?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Ok(()) => (),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    sync_parent(parent)
}
pub struct Journal {
    dir: PathBuf,
}
impl Journal {
    /// Open without submitting anything. Existing directories must not be symlinks.
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        create_dir(&dir, &|parent| fs::File::open(parent)?.sync_all())?;
        let meta = fs::symlink_metadata(&dir)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(io::Error::other("invalid journal directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
            fs::File::open(&dir)?.sync_all()?;
        }
        Ok(Self { dir })
    }
    fn path(&self, request: &str, suffix: &str) -> PathBuf {
        self.dir
            .join(format!("{:x}.{suffix}", Sha256::digest(request.as_bytes())))
    }
    fn install(&self, path: &Path, record: &Pending) -> io::Result<()> {
        let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        if bytes.len() as u64 > LIMIT {
            return Err(io::Error::other("journal record too large"));
        }
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir)?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        // Hard-link publication is atomic and cannot replace another writer's record.
        fs::hard_link(temp.path(), path)?;
        fs::File::open(&self.dir)?.sync_all()?;
        Ok(())
    }
    /// Durable barrier before the first network send. Failure means do not send.
    pub fn before_send(&self, endpoint: String, envelope: Vec<u8>) -> io::Result<Pending> {
        let record = Pending {
            endpoint,
            envelope,
            outcome: None,
        };
        let request = record
            .request()
            .map_err(|_| io::Error::other("invalid envelope"))?
            .id
            .key();
        self.install(&self.path(&request, "pending"), &record)?;
        Ok(record)
    }
    fn read_file(&self, path: &Path) -> io::Result<Pending> {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("invalid journal file"));
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            return Err(io::Error::other("journal record too large"));
        }
        let record: Pending = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let envelope = record
            .request()
            .map_err(|_| io::Error::other("invalid journal envelope"))?;
        if let Some(JournalOutcome::Recorded(outcome)) = &record.outcome {
            protocol::validate_recorded(&envelope, outcome)
                .map_err(|_| io::Error::other("invalid journal outcome"))?;
        }
        Ok(record)
    }
    /// Discover validated pending keys in deterministic lexical order, not timestamp order.
    /// Malformed pending records fail closed. Durable matching terminal records take precedence
    /// after a crash between terminal publication and pending unlink. Never submits or repairs.
    pub fn pending_keys(&self) -> io::Result<Vec<String>> {
        let mut keys = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "pending") {
                continue;
            }
            let pending = self.read_file(&path)?;
            let key = pending.request().map_err(io::Error::other)?.id.key();
            if path != self.path(&key, "pending") || pending.outcome.is_some() {
                return Err(io::Error::other("invalid pending record"));
            }
            match self.read_file(&self.path(&key, "terminal")) {
                Ok(terminal) => {
                    if terminal.endpoint != pending.endpoint
                        || terminal.envelope != pending.envelope
                        || terminal.outcome.is_none()
                    {
                        return Err(io::Error::other("conflicting terminal record"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => keys.push(key),
                Err(e) => return Err(e),
            }
        }
        keys.sort();
        Ok(keys)
    }
    /// Load only the requested original record. No directory scan or automatic retry.
    pub fn load(&self, request: &str) -> io::Result<Pending> {
        let record = match self.read_file(&self.path(request, "terminal")) {
            Ok(record) => record,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.read_file(&self.path(request, "pending"))?
            }
            Err(e) => return Err(e),
        };
        if record
            .request()
            .map_err(|_| io::Error::other("invalid envelope"))?
            .id
            .key()
            != request
        {
            return Err(io::Error::other("journal request mismatch"));
        }
        Ok(record)
    }
    /// Publish and fsync a terminal record *with its original context* before unlinking pending.
    /// Terminal records are retained for explicit recovery; this module does not prune them.
    pub fn finish(&self, pending: &Pending, outcome: protocol::RecordedOutcome) -> io::Result<()> {
        let envelope = pending.request().map_err(io::Error::other)?;
        protocol::validate_recorded(&envelope, &outcome)
            .map_err(|_| io::Error::other("invalid outcome"))?;
        self.finish_terminal(pending, JournalOutcome::Recorded(outcome))
    }
    /// Persist definitive non-application certified by a serialized server status read.
    pub fn finish_not_applied(&self, pending: &Pending) -> io::Result<()> {
        self.finish_terminal(pending, JournalOutcome::NotApplied)
    }
    fn finish_terminal(&self, pending: &Pending, outcome: JournalOutcome) -> io::Result<()> {
        let request = pending
            .request()
            .map_err(|_| io::Error::other("invalid envelope"))?
            .id
            .key();
        let mut terminal = pending.clone();
        terminal.outcome = Some(outcome);
        let path = self.path(&request, "terminal");
        match self.install(&path, &terminal) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let existing = self.read_file(&path)?;
                if existing.endpoint != terminal.endpoint
                    || existing.envelope != terminal.envelope
                    || existing.outcome != terminal.outcome
                {
                    return Err(io::Error::other("conflicting terminal record"));
                }
                fs::File::open(&self.dir)?.sync_all()?;
            }
            Err(e) => return Err(e),
        }
        match fs::remove_file(self.path(&request, "pending")) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        fs::File::open(&self.dir)?.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestor_sync_failure_stops_creation() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("first/second/journal");
        let result = create_dir(&leaf, &|parent| {
            assert_eq!(parent, root.path());
            Err(io::Error::other("injected parent fsync failure"))
        });
        assert!(result.is_err());
        assert!(root.path().join("first").is_dir());
        assert!(!root.path().join("first/second").exists());
    }
}
