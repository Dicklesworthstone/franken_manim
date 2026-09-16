//! Content-based source watching for the stable Studio development host.
//!
//! Polling is explicit and clock-injected: no background thread, scene callback
//! or platform-specific watcher is hidden inside this type. Only declared paths
//! are watched. Cargo output and .git directories are excluded from recursion.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{BuildError, ProtocolDigest, protocol_digest};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Missing,
    Directory,
    File(ProtocolDigest),
}

type Snapshot = Vec<(PathBuf, Input)>;

/// Work bounds applied to each complete source scan, before reads/expansion.
#[derive(Clone, Copy, Debug)]
pub struct WatchLimits {
    pub max_entries: usize,
    pub max_bytes: u64,
    pub max_depth: usize,
}
impl Default for WatchLimits {
    fn default() -> Self {
        Self { max_entries: 4096, max_bytes: 64 * 1024 * 1024, max_depth: 64 }
    }
}

/// Debounced source changes, including new/deleted files and atomic-save renames.
///
/// `poll` reports each stabilized changed content set once, before the caller
/// attempts its build. A failing build therefore does not cause an endless
/// rebuild loop; an edit made during the build is detected on the next poll.
/// Saving identical content or merely touching mtimes triggers no build.
/// A transient scan failure leaves both the prior snapshot and pending change
/// intact. This is change detection, not a sandbox or a Cargo dependency parser.
pub struct SourceWatch {
    inputs: Vec<PathBuf>,
    excludes: Vec<PathBuf>,
    limits: WatchLimits,
    debounce: Duration,
    observed: Snapshot,
    attempted: Snapshot,
    changed_at: Option<Duration>,
    last_poll: Option<Duration>,
}

impl SourceWatch {
    /// Watch explicit absolute files/directories. Missing inputs are recorded
    /// so a newly created source path will trigger a rebuild. Excluded absolute
    /// paths and their descendants are never traversed.
    pub fn new(
        inputs: Vec<PathBuf>, excludes: Vec<PathBuf>, debounce: Duration, limits: WatchLimits,
    ) -> Result<Self, BuildError> {
        if inputs.is_empty() || inputs.len() > limits.max_entries || excludes.len() > limits.max_entries
            || limits.max_entries == 0 || limits.max_entries > 65_536
            || limits.max_bytes == 0 || limits.max_bytes > 512 * 1024 * 1024
            || limits.max_depth == 0 || limits.max_depth > 128
            || inputs.iter().chain(&excludes).any(|path| !path.is_absolute())
        { return Err(error("invalid source-watch paths or work limits")); }
        let observed = scan(&inputs, &excludes, limits)?;
        Ok(Self {
            inputs, excludes, limits, debounce, attempted: observed.clone(), observed,
            changed_at: None, last_poll: None,
        })
    }

    /// Return true once a different content set has remained stable for the
    /// debounce interval. `now` is monotone elapsed time from the host's Clock.
    pub fn poll(&mut self, now: Duration) -> Result<bool, BuildError> {
        if self.last_poll.is_some_and(|last| now < last) {
            return Err(error("source-watch clock moved backwards"));
        }
        let current = scan(&self.inputs, &self.excludes, self.limits)?;
        self.last_poll = Some(now);
        if current != self.observed {
            self.observed = current;
            self.changed_at = Some(now);
        }
        if self.observed == self.attempted {
            self.changed_at = None;
            return Ok(false);
        }
        if self.changed_at.is_some_and(|changed| now.saturating_sub(changed) >= self.debounce) {
            self.attempted = self.observed.clone();
            self.changed_at = None;
            return Ok(true);
        }
        Ok(false)
    }
}

fn scan(inputs: &[PathBuf], excludes: &[PathBuf], limits: WatchLimits) -> Result<Snapshot, BuildError> {
    let mut work: Vec<_> = inputs.iter().cloned().map(|path| (path, 0)).collect();
    let mut seen = BTreeSet::new();
    let mut snapshot = Vec::new();
    let mut remaining = limits.max_bytes;
    while let Some((path, depth)) = work.pop() {
        if excludes.iter().any(|exclude| path.starts_with(exclude)) || seen.contains(&path) { continue; }
        if seen.len() >= limits.max_entries || depth > limits.max_depth {
            return Err(error("source-watch entry/depth budget exceeded"));
        }
        seen.insert(path.clone());
        snapshot.try_reserve(1).map_err(error)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                snapshot.push((path, Input::Missing)); continue;
            }
            Err(err) => return Err(error(err)),
        };
        if metadata.is_dir() {
            snapshot.push((path.clone(), Input::Directory));
            for child in fs::read_dir(&path).map_err(error)? {
                let child = child.map_err(error)?;
                let child_path = child.path();
                if matches!(child.file_name().to_str(), Some("target" | ".git"))
                    && child.file_type().map_err(error)?.is_dir()
                { continue; }
                if work.len().saturating_add(seen.len()) >= limits.max_entries {
                    return Err(error("source-watch pending-entry budget exceeded"));
                }
                work.try_reserve(1).map_err(error)?;
                work.push((child_path, depth + 1));
            }
        } else if metadata.is_file() {
            if metadata.len() > remaining { return Err(error("source-watch byte budget exceeded")); }
            let file = File::open(&path).map_err(error)?;
            if !file.metadata().map_err(error)?.is_file() { return Err(error("source changed type during watch scan")); }
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(usize::try_from(metadata.len()).map_err(error)?).map_err(error)?;
            file.take(remaining.saturating_add(1)).read_to_end(&mut bytes).map_err(error)?;
            if bytes.len() as u64 > remaining { return Err(error("source grew past the watch byte budget")); }
            remaining -= bytes.len() as u64;
            snapshot.push((path, Input::File(protocol_digest(&bytes))));
        } else {
            return Err(error(format!("source watcher refuses symlink or non-regular input: {}", path.display())));
        }
    }
    snapshot.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(snapshot)
}

fn error(value: impl std::fmt::Display) -> BuildError { BuildError::new(value.to_string()) }

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("fmn-watch-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&path).unwrap(); Self(path)
        }
        fn watch(&self) -> SourceWatch {
            SourceWatch::new(vec![self.0.clone()], Vec::new(), Duration::from_millis(100), WatchLimits::default()).unwrap()
        }
    }
    impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    fn ms(n: u64) -> Duration { Duration::from_millis(n) }

    #[test]
    fn stable_content_is_reported_once_and_failed_builds_do_not_spin() {
        let temp = Temp::new(); let source = temp.0.join("scene.rs"); fs::write(&source, "one").unwrap();
        let mut watch = temp.watch(); fs::write(&source, "two").unwrap();
        assert!(!watch.poll(ms(0)).unwrap()); assert!(!watch.poll(ms(99)).unwrap());
        assert!(watch.poll(ms(100)).unwrap()); assert!(!watch.poll(ms(1000)).unwrap());
        fs::write(&source, "two").unwrap(); assert!(!watch.poll(ms(1100)).unwrap());
        fs::write(&source, "edit while building").unwrap(); assert!(!watch.poll(ms(1200)).unwrap());
        assert!(watch.poll(ms(1300)).unwrap());
    }

    #[test]
    fn repeated_edits_debounce_and_reverts_cancel_pending_builds() {
        let temp = Temp::new(); let source = temp.0.join("scene.rs"); fs::write(&source, "one").unwrap();
        let mut watch = temp.watch(); fs::write(&source, "two").unwrap(); assert!(!watch.poll(ms(0)).unwrap());
        fs::write(&source, "three").unwrap(); assert!(!watch.poll(ms(90)).unwrap());
        assert!(!watch.poll(ms(100)).unwrap()); assert!(watch.poll(ms(190)).unwrap());
        fs::write(&source, "partial").unwrap(); assert!(!watch.poll(ms(200)).unwrap());
        fs::write(&source, "three").unwrap(); assert!(!watch.poll(ms(500)).unwrap());
    }

    #[test]
    fn directory_creation_deletion_and_output_exclusion() {
        let temp = Temp::new(); let mut watch = temp.watch();
        fs::create_dir(temp.0.join("target")).unwrap(); fs::write(temp.0.join("target/binary"), "not source").unwrap();
        assert!(!watch.poll(ms(0)).unwrap());
        fs::create_dir(temp.0.join("src")).unwrap(); fs::write(temp.0.join("src/new.rs"), "new").unwrap();
        assert!(!watch.poll(ms(1)).unwrap()); assert!(watch.poll(ms(101)).unwrap());
        fs::remove_file(temp.0.join("src/new.rs")).unwrap();
        assert!(!watch.poll(ms(102)).unwrap()); assert!(watch.poll(ms(202)).unwrap());
    }

    #[test]
    fn refused_scan_preserves_pending_change_and_clock_order() {
        let temp = Temp::new(); let source = temp.0.join("scene.rs"); fs::write(&source, "a").unwrap();
        let mut watch = SourceWatch::new(vec![source.clone()], Vec::new(), ms(100), WatchLimits { max_bytes: 2, ..WatchLimits::default() }).unwrap();
        fs::write(&source, "b").unwrap(); assert!(!watch.poll(ms(1)).unwrap());
        fs::write(&source, "too large").unwrap(); assert!(watch.poll(ms(150)).is_err());
        fs::write(&source, "b").unwrap(); assert!(watch.poll(ms(151)).unwrap());
        assert!(watch.poll(ms(100)).is_err()); assert!(!watch.poll(ms(200)).unwrap());
    }

    #[test]
    fn missing_explicit_file_is_watched_and_excluded_paths_do_not_trigger() {
        let temp = Temp::new(); let source = temp.0.join("later.rs");
        let mut watch = SourceWatch::new(vec![source.clone()], Vec::new(), Duration::ZERO, WatchLimits::default()).unwrap();
        fs::write(source, "new").unwrap(); assert!(watch.poll(ms(0)).unwrap());
        let generated = temp.0.join("generated"); fs::create_dir(&generated).unwrap();
        let mut watch = SourceWatch::new(vec![temp.0.clone()], vec![generated.clone()], Duration::ZERO, WatchLimits::default()).unwrap();
        fs::write(generated.join("output"), "x").unwrap(); assert!(!watch.poll(ms(0)).unwrap());
    }

    #[test]
    fn too_many_entries_refuse_instead_of_watching_an_incomplete_tree() {
        let temp = Temp::new(); fs::write(temp.0.join("a"), "").unwrap(); fs::write(temp.0.join("b"), "").unwrap();
        assert!(SourceWatch::new(vec![temp.0.clone()], Vec::new(), ms(100), WatchLimits { max_entries: 2, ..WatchLimits::default() }).is_err());
    }
}
