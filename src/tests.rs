// use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, LazyLock, Mutex};
use std::time::Duration;

use tempfile::TempDir;

use crate::events::EventType;
use crate::Watcher;

static TIMEOUT: LazyLock<Duration> =
    LazyLock::new(|| match std::env::var("FILESENTRY_TEST_TIMEOUT") {
        Ok(res) if !res.trim().is_empty() => Duration::from_secs(
            res.parse()
                .expect("invalid value for FILESENTRY_TEST_TIMEOUT expected an integer"),
        ),
        _ => Duration::from_secs(20),
    });

#[cfg(target_os = "linux")]
pub static READ_DELAY: LazyLock<Duration> =
    LazyLock::new(|| match std::env::var("FILESENTRY_READ_DELAY") {
        Ok(res) if !res.trim().is_empty() => Duration::from_millis(
            res.parse()
                .expect("invalid value for FILESENTRY_TEST_TIMEOUT expected an integer"),
        ),
        _ => Duration::from_millis(300),
    });

struct Assertion {
    done: mpsc::Receiver<()>,
    state: Arc<Mutex<Vec<(PathBuf, EventType)>>>,
    expected: Vec<(PathBuf, EventType)>,
}

impl Assertion {
    pub fn new<'a>(
        watcher: &Watcher,
        dir: &Path,
        expected: impl IntoIterator<Item = (&'a str, EventType)>,
    ) -> Assertion {
        let mut expected: Vec<_> = expected
            .into_iter()
            .map(|(path, event)| (dir.join(path), event))
            .collect();
        expected.sort_unstable();
        let state: Arc<Mutex<_>> = Arc::default();
        let (tx, rx) = mpsc::sync_channel(1);

        let len = expected.len();
        let assertion = Assertion {
            done: rx,
            state: state.clone(),
            expected,
        };

        watcher.add_handler(move |events| {
            if Arc::strong_count(&state) == 1 {
                return false;
            }
            let mut state = state.lock().unwrap();
            state.extend(
                events
                    .iter()
                    .map(|event| (event.path.as_std_path().to_owned(), event.ty)),
            );
            if state.len() >= len {
                let _ = tx.send(());
                false
            } else {
                true
            }
        });
        assertion
    }

    #[track_caller]
    pub fn check(self) {
        let timeout = self.done.recv_timeout(*TIMEOUT).is_err();
        self.state.clear_poison();
        let mut state = self.state.lock().unwrap();
        state.sort_unstable();
        if timeout {
            panic!("watcher didn't observer all changes within the timeout")
        } else {
            assert_eq!(&*state, &*self.expected)
        }
    }
}

fn rm_dir(dst: &Path, path: &str) {
    fs::remove_dir(dst.join(path)).unwrap();
}

fn rm_file(dst: &Path, path: &str) {
    fs::remove_file(dst.join(path)).unwrap();
}

fn write(dst: &Path, path: &str, content: &str) {
    fs::write(dst.join(path), content).unwrap();
}

fn mk_write(dst: &Path, path: &str, content: &str) {
    let path = dst.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn init_watcher() -> (TempDir, Watcher) {
    init_watcher_imp(false)
}

#[cfg(target_os = "linux")]
fn init_watcher_slow() -> (TempDir, Watcher) {
    init_watcher_imp(true)
}

fn init_watcher_imp(slow: bool) -> (TempDir, Watcher) {
    let _ = env_logger::builder().try_init();
    let dir = TempDir::new().unwrap();
    let watcher = Watcher::new_impl(slow).unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    watcher
        .add_root(dir.path(), true, move |success| {
            let _ = tx.send(success);
        })
        .unwrap();
    watcher.start();
    rx.recv_timeout(*TIMEOUT).expect("failed to start watcher");
    (dir, watcher)
}
fn with_watcher(f: impl FnOnce(&Path, &Watcher)) {
    let (dir, watcher) = init_watcher();
    let shutdown_guard = watcher.shutdown_guard();
    // Canonicalize to resolve symlinks (e.g., /var -> /private/var on macOS)
    // so assertion paths match the canonical paths used internally.
    let canonical = dir.path().canonicalize().unwrap();
    f(&canonical, &watcher);
    drop(shutdown_guard)
}

#[cfg(target_os = "linux")]
fn with_watcher_slow(f: impl FnOnce(&Path, &Watcher)) {
    let (dir, watcher) = init_watcher_slow();
    let shutdown_guard = watcher.shutdown_guard();
    f(dir.path(), &watcher);
    drop(shutdown_guard)
}

#[test]
fn create() {
    with_watcher(|dir, watcher| {
        let assertion = Assertion::new(
            watcher,
            dir,
            [
                ("foo/baz", EventType::Create),
                ("foo/bar/baz", EventType::Create),
                ("baz", EventType::Create),
            ],
        );
        mk_write(dir, "baz", "foo");
        mk_write(dir, "foo/baz", "foo");
        mk_write(dir, "foo/bar/baz", "foo");
        assertion.check();
    });
}

#[test]
fn delete() {
    with_watcher(|dir, watcher| {
        let assertion = Assertion::new(
            watcher,
            dir,
            [
                ("foo/baz", EventType::Create),
                ("foo/bar/baz", EventType::Create),
                ("baz", EventType::Create),
            ],
        );
        mk_write(dir, "baz", "foo");
        mk_write(dir, "foo/baz", "foo");
        mk_write(dir, "foo/bar/baz", "foo");
        assertion.check();
        let assertion = Assertion::new(watcher, dir, [("foo/bar/baz", EventType::Delete)]);
        rm_file(dir, "foo/bar/baz");
        rm_dir(dir, "foo/bar");
        assertion.check();
        let assertion = Assertion::new(watcher, dir, [("baz", EventType::Delete)]);
        rm_file(dir, "baz");
        assertion.check();
    });
}

#[test]
fn modify() {
    with_watcher(|dir, watcher| {
        let assertion = Assertion::new(
            watcher,
            dir,
            [
                ("foo/baz", EventType::Create),
                ("foo/bar/baz", EventType::Create),
                ("baz", EventType::Create),
            ],
        );
        mk_write(dir, "baz", "content1");
        mk_write(dir, "foo/baz", "content1");
        mk_write(dir, "foo/bar/baz", "content1");
        assertion.check();
        let assertion = Assertion::new(watcher, dir, [("foo/bar/baz", EventType::Modified)]);
        write(dir, "foo/bar/baz", "content2");
        assertion.check();
        let assertion = Assertion::new(watcher, dir, [("foo/baz", EventType::Modified)]);
        write(dir, "foo/baz", "content2");
        assertion.check();
        let assertion = Assertion::new(watcher, dir, [("baz", EventType::Modified)]);
        rm_file(dir, "baz");
        write(dir, "baz", "content3");
        assertion.check();
    });
}

// A watcher created before any root is added must park its event loop, not
// spin. Without a keep-alive source the macOS run loop returns immediately when
// nothing is scheduled, so the loop burns a core until the first root arrives.
#[test]
#[cfg(target_os = "macos")]
fn no_busy_loop_without_roots() {
    let _ = env_logger::builder().try_init();
    let watcher = Watcher::new_impl(false).unwrap();
    let guard = watcher.shutdown_guard();
    watcher.start();
    // Long enough that a spinning loop would rack up thousands of iterations.
    std::thread::sleep(Duration::from_millis(500));
    let spins = watcher.empty_iterations();
    drop(guard);
    assert!(
        spins < 50,
        "event loop spun {spins} times with no roots; expected it to park"
    );
}

// macOS has no slow-read knob to force a queue overflow on demand the way the
// inotify path does. This hammers the watcher with many files at once and
// asserts not a single create is lost. In practice it runs with zero recrawls
// because FSEvents absorbs the burst, so it does not actually exercise the
// MUST_SCAN_SUB_DIRS recrawl branch. That branch is driven directly by
// synthetic_rescan_does_not_duplicate instead. This stays as the macOS
// stand-in for queue_overflow under realistic load.
#[test]
#[cfg(target_os = "macos")]
fn bulk_create_no_lost_events() {
    with_watcher(|dir, watcher| {
        let files: Vec<_> = (0..20_000).map(|i| format!("foo{}/bar{i}", i % 200)).collect();
        let assertion = Assertion::new(
            watcher,
            dir,
            files.iter().map(|file| (&**file, EventType::Create)),
        );
        for file in &files {
            mk_write(dir, file, "content1");
        }
        assertion.check();
        eprintln!("bulk_create_no_lost_events observed all files after {} recrawls", watcher.recrawls());
    });
}

// A file created and then modified inside one settle window must surface as a
// single Create. FSEvents can coalesce CREATED and MODIFIED onto one event, and
// the if/else-if flag handling only reads the create flag. This confirms the
// create is never dropped, since the debouncer folds the modify into it.
#[test]
#[cfg(target_os = "macos")]
fn create_then_modify_coalesces_to_create() {
    with_watcher(|dir, watcher| {
        let assertion = Assertion::new(watcher, dir, [("a", EventType::Create)]);
        write(dir, "a", "v1");
        write(dir, "a", "v2");
        write(dir, "a", "v3");
        assertion.check();
    });
}

// A burst of modifies to an existing file must surface as a single Modified,
// not be dropped. Guards the steady-state side of the same flag handling.
#[test]
#[cfg(target_os = "macos")]
fn rapid_modify_of_existing_file() {
    with_watcher(|dir, watcher| {
        let assertion = Assertion::new(watcher, dir, [("a", EventType::Create)]);
        write(dir, "a", "v1");
        assertion.check();
        let assertion = Assertion::new(watcher, dir, [("a", EventType::Modified)]);
        write(dir, "a", "v2");
        write(dir, "a", "v3");
        write(dir, "a", "v4");
        assertion.check();
    });
}

// The MUST_SCAN_SUB_DIRS recrawl branch only fires when the FSEvents kernel
// buffer overflows, which bulk_create_no_lost_events shows we cannot provoke on
// demand. inject_rescan drives that exact branch so the recrawl path is
// actually exercised on macOS. A forced rescan clears pending changes and
// suppresses direct adds until the worker consumes it, so the create of d is
// rediscovered by the crawl itself. The exact-match assertion also catches the
// recrawl wrongly re-reporting files the tree already knows.
#[test]
#[cfg(target_os = "macos")]
fn synthetic_rescan_does_not_duplicate() {
    with_watcher(|dir, watcher| {
        let assertion = Assertion::new(
            watcher,
            dir,
            [
                ("a", EventType::Create),
                ("b", EventType::Create),
                ("c", EventType::Create),
            ],
        );
        write(dir, "a", "1");
        write(dir, "b", "1");
        write(dir, "c", "1");
        assertion.check();

        let assertion = Assertion::new(watcher, dir, [("d", EventType::Create)]);
        watcher.inject_rescan();
        write(dir, "d", "1");
        assertion.check();
        assert!(watcher.recrawls() >= 1, "expected the injected rescan to recrawl");
    });
}

#[test]
#[cfg(target_os = "linux")]
fn queue_overflow() {
    with_watcher_slow(|dir, watcher| {
        let files: Vec<_> = (0..20_0000)
            .map(|i| format!("foo{}/bar{i}", i % 200))
            .collect();
        let assertion = Assertion::new(
            watcher,
            dir,
            files.iter().map(|file| (&**file, EventType::Create)),
        );
        for file in &files {
            mk_write(dir, file, "content1");
        }
        assertion.check();
        let assertion = Assertion::new(
            watcher,
            dir,
            files.iter().map(|file| (&**file, EventType::Modified)),
        );
        for file in &files {
            write(dir, file, "content2");
        }
        assertion.check();
        let recrawls = watcher.recrawls();
        assert!(
            recrawls >= 2,
            "expected atleast 2 recrawls but found {recrawls}"
        )
    });
}
