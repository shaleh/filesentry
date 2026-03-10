use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{self, AtomicBool};
use std::sync::{Arc, Mutex};
use std::{io, thread};

mod sys;

use crate::path::CanonicalPathBuf;
use crate::pending::{self, PendingChangesLock};
use crate::{Filter, WatcherState};

use self::sys::{
    EventLoopState, SendableCFRunLoopRef, K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_CREATED,
    K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_INODE_META_MOD,
    K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_IS_DIR, K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_IS_FILE,
    K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_MODIFIED, K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_REMOVED,
    K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_RENAMED,
    K_FS_EVENT_STREAM_EVENT_FLAG_MUST_SCAN_SUB_DIRS,
    K_FS_EVENT_STREAM_EVENT_ID_SINCE_NOW,
};

pub(crate) struct FseventWatcher {
    run_loop: Mutex<Option<SendableCFRunLoopRef>>,
    event_loop_state: Arc<EventLoopState>,
    watched_roots: Mutex<Vec<CanonicalPathBuf>>,
    pub changes: PendingChangesLock,
}

impl std::fmt::Debug for FseventWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FseventWatcher")
            .field("shutdown", &self.event_loop_state.shutdown)
            .field("watched_roots", &self.watched_roots)
            .field("changes", &self.changes)
            .finish_non_exhaustive()
    }
}

impl FseventWatcher {
    pub fn shutdown(&self) {
        self.event_loop_state
            .shutdown
            .store(true, atomic::Ordering::Relaxed);
        self.wake_run_loop();
        self.changes.notify();
    }

    pub fn is_shutdown(&self) -> bool {
        self.event_loop_state
            .shutdown
            .load(atomic::Ordering::Relaxed)
    }

    pub fn new(
        #[cfg(test)] _slow: bool,
        state: Arc<WatcherState>,
    ) -> io::Result<Arc<Self>> {
        let event_loop_state = Arc::new(EventLoopState {
            needs_restart: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            last_event_id: std::sync::atomic::AtomicU64::new(
                K_FS_EVENT_STREAM_EVENT_ID_SINCE_NOW,
            ),
            stream_generation: std::sync::Mutex::new(0),
            stream_started: std::sync::Condvar::new(),
        });

        let watcher = Arc::new(Self {
            run_loop: Mutex::new(None),
            event_loop_state: event_loop_state.clone(),
            watched_roots: Mutex::new(Vec::new()),
            changes: PendingChangesLock::default(),
        });

        let filter: Arc<Mutex<Arc<dyn Filter>>> =
            Arc::new(Mutex::new(state.config.lock().unwrap().filter.clone()));
        let watcher_ = watcher.clone();

        thread::spawn(move || {
            sys::event_loop(
                event_loop_state,
                &watcher_.run_loop,
                // get_paths: return null-terminated UTF-8 paths
                {
                    let watcher = watcher_.clone();
                    move || {
                        let roots = watcher.watched_roots.lock().unwrap();
                        roots
                            .iter()
                            .map(|p: &CanonicalPathBuf| {
                                let mut bytes = p.as_os_str().as_bytes().to_vec();
                                bytes.push(0); // null-terminate for C
                                bytes
                            })
                            .collect()
                    }
                },
                // handler: process each event
                {
                    let watcher = watcher_.clone();
                    let filter = filter.clone();
                    move |path_bytes: &[u8], flags: sys::FSEventStreamEventFlags| {
                        let filter = filter.lock().unwrap();
                        watcher.handle_event(path_bytes, flags, &**filter);
                    }
                },
                // notify: signal pending changes
                {
                    let watcher = watcher_.clone();
                    move || {
                        watcher.changes.notify();
                    }
                },
                // handle_message: refresh config, check shutdown
                {
                    let watcher = watcher_.clone();
                    move || {
                        *filter.lock().unwrap() =
                            state.config.lock().unwrap().filter.clone();
                        watcher.is_shutdown()
                    }
                },
            );
        });

        Ok(watcher)
    }

    pub fn watch_dir(&self, path: CanonicalPathBuf) -> io::Result<()> {
        let mut roots = self.watched_roots.lock().unwrap();

        // FSEvents is inherently recursive, so check if this path is already
        // covered by an existing root
        for root in roots.iter() {
            if root.is_parent_of(&path) || *root == path {
                return Ok(());
            }
        }

        // Check if new root covers existing roots and remove them
        roots.retain(|existing| !path.is_parent_of(existing));

        roots.push(path);
        drop(roots);

        // Read current generation before triggering restart
        let gen = self.event_loop_state.stream_generation.lock().unwrap();
        let target = *gen + 1;
        drop(gen);

        self.event_loop_state
            .needs_restart
            .store(true, atomic::Ordering::Relaxed);
        self.wake_run_loop();

        // Wait until the event loop has actually (re)started the stream
        let gen = self.event_loop_state.stream_generation.lock().unwrap();
        let _gen = self
            .event_loop_state
            .stream_started
            .wait_while(gen, |g| *g < target)
            .unwrap();
        Ok(())
    }

    pub fn refresh_config(&self) {
        self.wake_run_loop();
    }

    fn wake_run_loop(&self) {
        let guard = self.run_loop.lock().unwrap();
        if let Some(ref rl) = *guard {
            unsafe {
                sys::CFRunLoopStop(rl.0);
            }
        }
    }

    fn handle_event(
        &self,
        path_bytes: &[u8],
        flags: sys::FSEventStreamEventFlags,
        filter: &dyn Filter,
    ) {
        if flags & K_FS_EVENT_STREAM_EVENT_FLAG_MUST_SCAN_SUB_DIRS != 0 {
            self.changes.lock().recrawl();
            return;
        }

        let path_os = OsStr::from_bytes(path_bytes);
        let path = CanonicalPathBuf::assert_canonicalized(path_os.as_ref());

        let is_dir = if flags & K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_IS_DIR != 0 {
            Some(true)
        } else if flags & K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_IS_FILE != 0 {
            Some(false)
        } else {
            None
        };

        if filter.ignore_path(path.as_std_path(), is_dir) {
            return;
        }

        let mut pending = self.changes.lock();
        if flags
            & (K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_CREATED
                | K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_REMOVED
                | K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_RENAMED)
            != 0
        {
            pending.add_watcher(path, pending::Flags::NEEDS_RECURSIVE_CRAWL);
        } else if flags
            & (K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_MODIFIED
                | K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_INODE_META_MOD)
            != 0
        {
            pending.add_watcher(path, pending::Flags::empty());
        }
    }
}
