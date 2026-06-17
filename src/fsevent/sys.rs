use std::ffi::{c_char, c_double, c_void, CStr};
use std::ptr;
use std::sync::atomic::{self, AtomicBool, AtomicU64};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Condvar, Mutex};

// Core Foundation types
#[repr(C)]
pub struct __CFRunLoop(c_void);
pub type CFRunLoopRef = *mut __CFRunLoop;

#[repr(C)]
pub struct __CFRunLoopSource(c_void);
pub type CFRunLoopSourceRef = *mut __CFRunLoopSource;

#[repr(C)]
pub struct __CFAllocator(c_void);
pub type CFAllocatorRef = *const __CFAllocator;

#[repr(C)]
pub struct __CFString(c_void);
pub type CFStringRef = *const __CFString;

#[repr(C)]
pub struct __CFArray(c_void);
pub type CFArrayRef = *const __CFArray;

pub type CFIndex = isize;
pub type CFStringEncoding = u32;
pub const K_CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x08000100;

// FSEvents types
#[repr(C)]
pub struct __FSEventStream(c_void);
pub type FSEventStreamRef = *mut __FSEventStream;

pub type FSEventStreamEventFlags = u32;
pub type FSEventStreamEventId = u64;
pub type FSEventStreamCreateFlags = u32;

// Stream creation flags
pub const K_FS_EVENT_STREAM_CREATE_FLAG_FILE_EVENTS: FSEventStreamCreateFlags = 0x00000010;
pub const K_FS_EVENT_STREAM_CREATE_FLAG_NO_DEFER: FSEventStreamCreateFlags = 0x00000002;

// Event flags
pub const K_FS_EVENT_STREAM_EVENT_FLAG_MUST_SCAN_SUB_DIRS: FSEventStreamEventFlags = 0x00000001;
pub const K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_CREATED: FSEventStreamEventFlags = 0x00000100;
pub const K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_REMOVED: FSEventStreamEventFlags = 0x00000200;
pub const K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_INODE_META_MOD: FSEventStreamEventFlags = 0x00000400;
pub const K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_RENAMED: FSEventStreamEventFlags = 0x00000800;
pub const K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_MODIFIED: FSEventStreamEventFlags = 0x00001000;
pub const K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_IS_FILE: FSEventStreamEventFlags = 0x00010000;
pub const K_FS_EVENT_STREAM_EVENT_FLAG_ITEM_IS_DIR: FSEventStreamEventFlags = 0x00020000;

pub const K_FS_EVENT_STREAM_EVENT_ID_SINCE_NOW: FSEventStreamEventId = 0xFFFFFFFFFFFFFFFF;

// CFArray callback structure
#[repr(C)]
pub struct CFArrayCallBacks {
    pub version: CFIndex,
    pub retain: *const c_void,
    pub release: *const c_void,
    pub copy_description: *const c_void,
    pub equal: *const c_void,
}

// CFRunLoopSourceContext for a version 0 source. Only the perform callback is
// set. The source is never signaled. It exists so the run loop always has an
// input source and CFRunLoopRun blocks instead of returning immediately.
#[repr(C)]
pub struct CFRunLoopSourceContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<extern "C" fn(*const c_void)>,
    pub copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
    pub equal: Option<extern "C" fn(*const c_void, *const c_void) -> u8>,
    pub hash: Option<extern "C" fn(*const c_void) -> usize>,
    pub schedule: Option<extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    pub cancel: Option<extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    pub perform: Option<extern "C" fn(*mut c_void)>,
}

// FSEventStreamContext
#[repr(C)]
pub struct FSEventStreamContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<extern "C" fn(*const c_void)>,
    pub copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

pub type FSEventStreamCallback = extern "C" fn(
    stream_ref: FSEventStreamRef,
    client_callback_info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const FSEventStreamEventFlags,
    event_ids: *const FSEventStreamEventId,
);

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub static kCFAllocatorDefault: CFAllocatorRef;

    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopStop(rl: CFRunLoopRef);

    pub fn CFRunLoopSourceCreate(
        allocator: CFAllocatorRef,
        order: CFIndex,
        context: *mut CFRunLoopSourceContext,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopAddSource(
        rl: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        mode: CFStringRef,
    );

    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: CFStringEncoding,
    ) -> CFStringRef;
    pub fn CFRelease(cf: *const c_void);

    pub fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const *const c_void,
        num_values: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;

    pub fn FSEventStreamCreate(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *mut FSEventStreamContext,
        paths_to_watch: CFArrayRef,
        since_when: FSEventStreamEventId,
        latency: c_double,
        flags: FSEventStreamCreateFlags,
    ) -> FSEventStreamRef;
    pub fn FSEventStreamScheduleWithRunLoop(
        stream_ref: FSEventStreamRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
    );
    pub fn FSEventStreamStart(stream_ref: FSEventStreamRef) -> bool;
    pub fn FSEventStreamStop(stream_ref: FSEventStreamRef);
    pub fn FSEventStreamInvalidate(stream_ref: FSEventStreamRef);
    pub fn FSEventStreamRelease(stream_ref: FSEventStreamRef);
}

// Safety: CFRunLoopRef can be sent to other threads for CFRunLoopStop
pub struct SendableCFRunLoopRef(pub CFRunLoopRef);
unsafe impl Send for SendableCFRunLoopRef {}
unsafe impl Sync for SendableCFRunLoopRef {}

/// State shared between the event loop thread and the callback.
pub(super) struct EventLoopState {
    pub needs_restart: AtomicBool,
    pub shutdown: AtomicBool,
    pub last_event_id: AtomicU64,
    /// Incremented each time the stream is (re)started. Used with `stream_started`
    /// to let `watch_dir` wait until the stream is actually running.
    pub stream_generation: Mutex<u64>,
    pub stream_started: Condvar,
    /// Counts how many times the event loop passed through the no-paths branch.
    /// A healthy loop parks there and barely moves. A busy loop spins it.
    #[cfg(test)]
    pub empty_iterations: AtomicUsize,
}

/// Creates a CFArray of CFStringRef from the given paths.
/// The caller is responsible for releasing the returned CFArrayRef and the individual CFStringRefs.
unsafe fn create_cf_paths(
    paths: &[Vec<u8>],
) -> (CFArrayRef, Vec<CFStringRef>) {
    let cf_strings: Vec<CFStringRef> = paths
        .iter()
        .map(|p| {
            CFStringCreateWithCString(
                kCFAllocatorDefault,
                p.as_ptr() as *const c_char,
                K_CF_STRING_ENCODING_UTF8,
            )
        })
        .collect();
    let ptrs: Vec<*const c_void> = cf_strings.iter().map(|s| *s as *const c_void).collect();
    // Use kCFTypeArrayCallBacks equivalent: retain/release for CF types
    let callbacks = CFArrayCallBacks {
        version: 0,
        retain: ptr::null(),
        release: ptr::null(),
        copy_description: ptr::null(),
        equal: ptr::null(),
    };
    let array = CFArrayCreate(
        kCFAllocatorDefault,
        ptrs.as_ptr(),
        ptrs.len() as CFIndex,
        &callbacks,
    );
    (array, cf_strings)
}

/// The raw C callback invoked by FSEvents.
extern "C" fn fsevent_callback(
    _stream_ref: FSEventStreamRef,
    client_callback_info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const FSEventStreamEventFlags,
    event_ids: *const FSEventStreamEventId,
) {
    unsafe {
        let info = &*(client_callback_info as *const CallbackInfo);
        let paths = std::slice::from_raw_parts(event_paths as *const *const c_char, num_events);
        let flags = std::slice::from_raw_parts(event_flags, num_events);
        let ids = std::slice::from_raw_parts(event_ids, num_events);

        for (&path_ptr, &flag) in paths.iter().zip(flags) {
            let path = CStr::from_ptr(path_ptr).to_bytes();
            (info.handler)(path, flag);
        }

        // Track the latest event ID for gap-free restarts
        if let Some(&max_id) = ids.last() {
            info.state.last_event_id.store(max_id, atomic::Ordering::Relaxed);
        }

        (info.notify)();
    }
}

type EventHandler = Box<dyn Fn(&[u8], FSEventStreamEventFlags) + Send + Sync>;
type NotifyFn = Box<dyn Fn() + Send + Sync>;

struct CallbackInfo {
    handler: EventHandler,
    notify: NotifyFn,
    state: Arc<EventLoopState>,
}

// Perform callback for the keep-alive source. The source is never signaled so
// this never runs. It only needs to exist to satisfy the source contract.
extern "C" fn noop_perform(_info: *mut c_void) {}

/// Run the FSEvents event loop on the current thread.
/// `run_loop_out` receives the CFRunLoopRef once the loop starts so other threads can stop it.
/// `get_paths` returns the current set of paths to watch (as null-terminated UTF-8 byte vectors).
/// `handler` is called for each event with (path_bytes, event_flags).
/// `notify` is called after processing a batch of events.
/// `handle_message` is called when the run loop is woken; returns true to shut down.
pub(super) fn event_loop(
    state: Arc<EventLoopState>,
    run_loop_out: &std::sync::Mutex<Option<SendableCFRunLoopRef>>,
    get_paths: impl Fn() -> Vec<Vec<u8>>,
    handler: impl Fn(&[u8], FSEventStreamEventFlags) + Send + Sync + 'static,
    notify: impl Fn() + Send + Sync + 'static,
    mut handle_message: impl FnMut() -> bool,
) {
    unsafe {
        let current_loop = CFRunLoopGetCurrent();
        *run_loop_out.lock().unwrap() = Some(SendableCFRunLoopRef(current_loop));

        // Add a keep-alive source so the run loop always has an input source.
        // Without it CFRunLoopRun returns immediately whenever no stream is
        // scheduled, which spins the loop until the first root is watched.
        let mut keepalive_ctx = CFRunLoopSourceContext {
            version: 0,
            info: ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(noop_perform),
        };
        let keepalive = CFRunLoopSourceCreate(kCFAllocatorDefault, 0, &mut keepalive_ctx);
        CFRunLoopAddSource(current_loop, keepalive, kCFRunLoopDefaultMode);

        let callback_info = Box::new(CallbackInfo {
            handler: Box::new(handler),
            notify: Box::new(notify),
            state: state.clone(),
        });
        let info_ptr = Box::into_raw(callback_info);

        loop {
            let paths = get_paths();
            if paths.is_empty() {
                #[cfg(test)]
                state
                    .empty_iterations
                    .fetch_add(1, atomic::Ordering::Relaxed);
                // No paths to watch yet. Park on the run loop until something wakes us.
                CFRunLoopRun();
                if state.shutdown.load(atomic::Ordering::Relaxed) {
                    break;
                }
                if handle_message() {
                    break;
                }
                continue;
            }

            let since_when = state.last_event_id.load(atomic::Ordering::Relaxed);

            let (cf_paths, cf_strings) = create_cf_paths(&paths);

            let mut context = FSEventStreamContext {
                version: 0,
                info: info_ptr as *mut c_void,
                retain: None,
                release: None,
                copy_description: None,
            };

            let stream = FSEventStreamCreate(
                kCFAllocatorDefault,
                fsevent_callback,
                &mut context,
                cf_paths,
                since_when,
                0.0, // latency — immediate delivery
                K_FS_EVENT_STREAM_CREATE_FLAG_FILE_EVENTS
                    | K_FS_EVENT_STREAM_CREATE_FLAG_NO_DEFER,
            );

            FSEventStreamScheduleWithRunLoop(stream, current_loop, kCFRunLoopDefaultMode);
            if !FSEventStreamStart(stream) {
                // The stream failed to start, so no events will arrive from it.
                // We still advance the generation below. A waiting watch_dir
                // must be released rather than blocked forever on a dead stream.
                log::error!("FSEventStreamStart failed; file events will not be delivered");
            }

            // Signal that the stream is running so watch_dir can proceed
            {
                let mut gen = state.stream_generation.lock().unwrap();
                *gen += 1;
            }
            state.stream_started.notify_all();

            // Release the CF path objects now that the stream owns them
            for s in &cf_strings {
                CFRelease(*s as *const c_void);
            }
            CFRelease(cf_paths as *const c_void);

            // Block until CFRunLoopStop is called
            CFRunLoopRun();

            // Tear down the stream
            FSEventStreamStop(stream);
            FSEventStreamInvalidate(stream);
            FSEventStreamRelease(stream);

            if state.shutdown.load(atomic::Ordering::Relaxed) {
                break;
            }

            if handle_message() {
                break;
            }

            // Clear restart flag before looping to recreate stream with new paths
            state.needs_restart.store(false, atomic::Ordering::Relaxed);
        }

        // Clean up the keep-alive source and callback info.
        CFRelease(keepalive as *const c_void);
        drop(Box::from_raw(info_ptr));
    }
}
