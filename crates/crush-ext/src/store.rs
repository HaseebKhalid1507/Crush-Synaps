//! The raw store: where the shim keeps a tool's raw output before it hands
//! Synaps a lossy replacement, so every crush marker points at a file that
//! exists and holds exactly what the tool printed (as the hook saw it).
//!
//! Layout: `<root>/<session>/<hash>.txt`, `<root>` = `$XDG_CACHE_HOME/crush/raw`
//! (else `~/.cache/crush/raw`), `<session>` the hook's `session_id` reduced to
//! `[A-Za-z0-9_-]` (or `nosession`), `<hash>` 16 hex digits of a stable hash
//! of the output. Directories are 0700, files 0600. It doesn't use Synaps'
//! spill directory: that layout is Synaps' business.
//!
//! - **Atomic, no clobber.** The raw goes to a fresh temp file in the same
//!   directory (`O_CREAT|O_EXCL`, so a planted symlink fails the create), then
//!   is hard-linked to its final name, which fails rather than replace an
//!   existing file. The final name is visible complete or not at all.
//! - **Content-addressed.** The same output in the same session maps to the
//!   same path, so a re-run yields byte-identical crushed text (cache-stable).
//!   An existing file is reused only if its bytes are equal; a different file
//!   under that name (a hash collision) is an error: the caller passes through.
//! - **Retention.** [`sweep`] deletes kept files older than N days, then the
//!   oldest until the store is under its size cap. It only ever touches names
//!   this module creates, one directory level deep, never following symlinks.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Files the size cap may not delete yet (a live session may have just
/// handed out their path).
pub const CAP_GRACE: Duration = Duration::from_secs(600);
/// Temp files older than this are leftovers of a crash.
const TMP_GRACE: Duration = Duration::from_secs(3600);

/// A stable 64-bit hash (word at a time, murmur3 finaliser). Not a security
/// boundary: [`RawStore::keep`] compares bytes before it reuses a file.
pub fn hash64(b: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    const M: u64 = 0xBF58_476D_1CE4_E5B9;
    let mut h = 0x243F_6A88_85A3_08D3u64 ^ (b.len() as u64).wrapping_mul(K);
    let mut chunks = b.chunks_exact(8);
    for c in &mut chunks {
        let w = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        h = (h ^ w.wrapping_mul(K)).rotate_left(29).wrapping_mul(M);
    }
    let rem = chunks.remainder();
    let mut tail = [0u8; 8];
    tail[..rem.len()].copy_from_slice(rem);
    h ^= u64::from_le_bytes(tail).wrapping_mul(K);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^ (h >> 33)
}

/// A session id as one safe path component: `[A-Za-z0-9_-]`, at most 64
/// bytes; missing or empty → `nosession`.
pub fn session_component(id: Option<&str>) -> String {
    let s: String = id
        .unwrap_or("")
        .chars()
        .take(64)
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if s.is_empty() {
        "nosession".into()
    } else {
        s
    }
}

/// `$XDG_CACHE_HOME/crush` (absolute only, per the XDG spec), else
/// `$HOME/.cache/crush`. Synaps forwards `HOME` but not `XDG_CACHE_HOME` to
/// extensions, so under Synaps this is normally `~/.cache/crush`.
pub fn default_state_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(x.join("crush"));
    }
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute()).map(|h| h.join(".cache").join("crush"))
}

/// Create `dir` (and missing parents) 0700; refuse a symlink or non-directory.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)?;
    let m = std::fs::symlink_metadata(dir)?;
    if !m.file_type().is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a directory", dir.display())));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if m.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct RawStore {
    root: PathBuf,
    /// `--keep-raw-dir`: files directly in `root`, no session directories.
    flat: bool,
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

impl RawStore {
    pub fn new(root: PathBuf, flat: bool) -> Self {
        Self { root, flat }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the raw of `output` (in `session`) is kept. Pure: the same
    /// inputs give the same path.
    pub fn path_for(&self, session: Option<&str>, output: &str) -> PathBuf {
        let name = format!("{:016x}.txt", hash64(output.as_bytes()));
        if self.flat {
            self.root.join(name)
        } else {
            self.root.join(session_component(session)).join(name)
        }
    }

    /// Keep `raw` at `path` (from [`Self::path_for`]). On `Ok` the file exists,
    /// is complete, 0600, and holds exactly `raw`.
    pub fn keep(&self, path: &Path, raw: &str) -> io::Result<()> {
        let dir = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent"))?;
        if !self.flat {
            ensure_private_dir(&self.root)?;
        }
        ensure_private_dir(dir)?;
        if same_file_content(path, raw)? {
            touch(path);
            return Ok(());
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("raw");
        let tmp = dir.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let res = (|| {
            let mut f = create_new_private(&tmp)?;
            f.write_all(raw.as_bytes())?;
            f.flush()?;
            drop(f);
            match std::fs::hard_link(&tmp, path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    // raced with another writer of the same output, or a collision
                    if same_file_content(path, raw)? {
                        Ok(())
                    } else {
                        Err(io::Error::new(io::ErrorKind::AlreadyExists, "a different raw is kept under this name"))
                    }
                }
                // a filesystem without hard links: rename, which never
                // clobbers here because the name was free a moment ago
                Err(_) if std::fs::symlink_metadata(path).is_err() => std::fs::rename(&tmp, path),
                Err(e) => Err(e),
            }
        })();
        let _ = std::fs::remove_file(&tmp);
        res
    }
}

/// Does `path` exist as a regular file with exactly `raw`'s bytes? `Err` when
/// it exists but isn't a regular file (or can't be read).
fn same_file_content(path: &Path, raw: &str) -> io::Result<bool> {
    let m = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if !m.file_type().is_file() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "not a regular file"));
    }
    if m.len() != raw.len() as u64 {
        return Ok(false);
    }
    let mut buf = Vec::with_capacity(raw.len());
    std::fs::File::open(path)?.read_to_end(&mut buf)?;
    Ok(buf == raw.as_bytes())
}

/// Retention counts from the last time a raw was handed out.
fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

fn create_new_private(path: &Path) -> io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    // O_CREAT|O_EXCL: never follows a symlink, never reuses a file
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// A kept raw (`<16 hex>.txt`) or one of our temp files (`.<that>.<pid>.<n>.tmp`).
fn ours(name: &str) -> Option<bool> {
    let is_raw = |n: &str| n.len() == 20 && n.ends_with(".txt") && n[..16].bytes().all(|b| b.is_ascii_hexdigit());
    if is_raw(name) {
        return Some(false);
    }
    let t = name.strip_prefix('.')?.strip_suffix(".tmp")?;
    is_raw(t.get(..20)?).then_some(true)
}

fn session_dir_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub deleted: u64,
    pub freed: u64,
    pub left: Usage,
}

struct Entry {
    path: PathBuf,
    len: u64,
    mtime: SystemTime,
    tmp: bool,
}

/// Our files under `root` (top level and one session level down).
fn entries(root: &Path) -> (Vec<Entry>, Vec<PathBuf>) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let scan = |dir: &Path, files: &mut Vec<Entry>, dirs: Option<&mut Vec<PathBuf>>| {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut dirs = dirs;
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(m) = e.metadata() else { continue }; // lstat: DirEntry doesn't follow
            if m.file_type().is_file() {
                if let Some(tmp) = ours(name) {
                    files.push(Entry {
                        path: e.path(),
                        len: m.len(),
                        mtime: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        tmp,
                    });
                }
            } else if m.file_type().is_dir() && session_dir_name(name) {
                if let Some(d) = dirs.as_deref_mut() {
                    d.push(e.path());
                }
            }
        }
    };
    scan(root, &mut files, Some(&mut dirs));
    for d in &dirs {
        scan(d, &mut files, None);
    }
    (files, dirs)
}

/// What the store holds now.
pub fn usage(root: &Path) -> Usage {
    let (files, _) = entries(root);
    Usage { files: files.iter().filter(|e| !e.tmp).count() as u64, bytes: files.iter().map(|e| e.len).sum() }
}

/// Delete raws older than `max_age`, then the oldest until the store is at
/// most `max_bytes` (files younger than [`CAP_GRACE`] are kept even over the
/// cap), stale temp files, and session directories left empty.
pub fn sweep(root: &Path, max_age: Duration, max_bytes: u64, now: SystemTime) -> SweepReport {
    let (mut files, dirs) = entries(root);
    let age = |e: &Entry| now.duration_since(e.mtime).unwrap_or(Duration::ZERO);
    let mut r = SweepReport::default();
    let del = |e: &Entry, r: &mut SweepReport| {
        if std::fs::remove_file(&e.path).is_ok() {
            r.deleted += u64::from(!e.tmp);
            r.freed += e.len;
            true
        } else {
            false
        }
    };
    files.retain(|e| {
        let expired = if e.tmp { age(e) > TMP_GRACE } else { age(e) > max_age };
        !(expired && del(e, &mut r))
    });
    let mut total: u64 = files.iter().map(|e| e.len).sum();
    if total > max_bytes {
        files.sort_by(|a, b| a.mtime.cmp(&b.mtime).then_with(|| a.path.cmp(&b.path)));
        let mut keep = Vec::with_capacity(files.len());
        for e in files {
            if total > max_bytes && !e.tmp && age(&e) > CAP_GRACE && del(&e, &mut r) {
                total -= e.len;
            } else {
                keep.push(e);
            }
        }
        files = keep;
    }
    for d in dirs {
        let _ = std::fs::remove_dir(d); // only succeeds when empty
    }
    r.left = Usage { files: files.iter().filter(|e| !e.tmp).count() as u64, bytes: total };
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        #[cfg(unix)]
        {
            std::fs::metadata(p).unwrap().permissions().mode() & 0o777
        }
        #[cfg(not(unix))]
        {
            let _ = p;
            0o600
        }
    }

    fn age_file(p: &Path, by: Duration) {
        let t = SystemTime::now() - by;
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn hash_is_stable_and_spreads() {
        assert_eq!(hash64(b""), hash64(b""));
        assert_eq!(format!("{:016x}", hash64(b"hello world\n")), format!("{:016x}", hash64(b"hello world\n")));
        assert_ne!(hash64(b"a"), hash64(b"b"));
        assert_ne!(hash64(b"abcdefgh"), hash64(b"abcdefgh\0"));
        assert_ne!(hash64(b"12345678abcdefgh"), hash64(b"abcdefgh12345678"));
    }

    #[test]
    fn session_components_are_safe() {
        assert_eq!(session_component(None), "nosession");
        assert_eq!(session_component(Some("")), "nosession");
        assert_eq!(session_component(Some("../../etc")), "______etc");
        assert_eq!(session_component(Some("sess-1_a")), "sess-1_a");
        assert_eq!(session_component(Some(&"x".repeat(200))).len(), 64);
    }

    #[test]
    fn keep_is_private_atomic_and_content_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("crush").join("raw");
        let s = RawStore::new(root.clone(), false);
        let p = s.path_for(Some("sess/1"), "raw output\n");
        assert_eq!(p.parent().unwrap(), root.join("sess_1"));
        s.keep(&p, "raw output\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "raw output\n");
        assert_eq!(mode(&p), 0o600);
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(p.parent().unwrap()), 0o700);
        // the same output again: reused
        s.keep(&p, "raw output\n").unwrap();
        // a different file under that name (collision): refused, not clobbered
        assert!(s.keep(&p, "something else\n").is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "raw output\n");
        // no temp files left behind
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_is_never_followed_or_clobbered() {
        let tmp = tempfile::tempdir().unwrap();
        let s = RawStore::new(tmp.path().join("raw"), false);
        let p = s.path_for(None, "secret raw\n");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let target = tmp.path().join("victim");
        std::fs::write(&target, "victim").unwrap();
        std::os::unix::fs::symlink(&target, &p).unwrap();
        assert!(s.keep(&p, "secret raw\n").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "victim");
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_store_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("raw");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("nosession"), "a file where the dir should be").unwrap();
        let s = RawStore::new(root, false);
        let p = s.path_for(None, "x\n");
        assert!(s.keep(&p, "x\n").is_err());
    }

    #[test]
    fn sweep_deletes_old_then_oldest_over_the_cap_and_only_our_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("raw");
        let s = RawStore::new(root.clone(), false);
        let mk = |sess: &str, body: &str, age: Duration| {
            let p = s.path_for(Some(sess), body);
            s.keep(&p, body).unwrap();
            age_file(&p, age);
            p
        };
        let day = Duration::from_secs(86_400);
        let old = mk("a", &"o".repeat(1000), day * 8);
        let mid = mk("b", &"m".repeat(1000), day * 3);
        let newer = mk("b", &"n".repeat(1000), day);
        let fresh = mk("c", &"f".repeat(1000), Duration::from_secs(5));
        let foreign = root.join("b").join("notes.txt");
        std::fs::write(&foreign, "not ours").unwrap();
        age_file(&foreign, day * 30);
        let r = sweep(&root, day * 7, 2500, SystemTime::now());
        assert!(!old.exists(), "older than 7 days");
        assert!(!mid.exists(), "oldest over the cap");
        assert!(newer.exists() && fresh.exists());
        assert!(foreign.exists(), "never touches a file it didn't create");
        assert!(!root.join("a").exists(), "empty session dir removed");
        assert_eq!((r.deleted, r.left.files, r.left.bytes), (2, 2, 2000));
        // over the cap but within the grace period: kept
        let r = sweep(&root, day * 7, 10, SystemTime::now());
        assert!(fresh.exists());
        assert!(!newer.exists());
        assert_eq!(r.left, Usage { files: 1, bytes: 1000 });
        assert_eq!(usage(&root), Usage { files: 1, bytes: 1000 });
    }
}
