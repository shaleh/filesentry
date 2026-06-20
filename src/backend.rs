//! The native file-watching backend abstraction.
//!
//! The worker and [`crate::Watcher`] are written against this trait so a backend
//! can be selected per target OS ([`NativeBackend`]) instead of hardcoding
//! `InotifyWatcher`. A backend owns a [`PendingChangesLock`] that it pushes raw
//! change notifications into (and whose condvar it signals). The worker drains it,
//! re-stats the affected paths and diffs them against the in-memory tree. This is
//! the entire surface the rest of the crate needs.

use std::io;

use crate::path::CanonicalPathBuf;
use crate::pending::PendingChangesLock;

pub(crate) trait Backend: Send + Sync + 'static {
    /// The queue the worker drains; the backend pushes into it and signals its
    /// condvar (`changes().notify()`) when new notifications arrive.
    fn changes(&self) -> &PendingChangesLock;

    /// Start watching a directory. `recursive` asks the backend to cover the whole
    /// subtree: subtree backends (`ReadDirectoryChangesW`) honor it; per-directory
    /// backends (inotify) ignore it and rely on the worker calling `watch_dir` per
    /// directory. Recursive backends no-op a path already covered by an ancestor.
    fn watch_dir(&self, path: CanonicalPathBuf, recursive: bool) -> io::Result<()>;

    /// Request shutdown of the backend's worker thread(s) and wake the worker.
    fn shutdown(&self);
    fn is_shutdown(&self) -> bool;

    /// Re-read live configuration (e.g. the filter); wake the event loop.
    fn refresh_config(&self);
}

// Selected per OS; , otherwise the watcher is compiled out and helix falls back to polling.
#[cfg(target_os = "linux")]
pub(crate) type NativeBackend = crate::inotify::InotifyWatcher;
#[cfg(target_os = "macos")]
pub(crate) type NativeBackend = crate::fsevent::FsEventWatcher;
#[cfg(target_os = "windows")]
pub(crate) type NativeBackend = crate::windows::WindowsWatcher;
