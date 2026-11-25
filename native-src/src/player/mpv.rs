//! Minimal dynamic binding to libmpv (client API + software render API).
//! libmpv-2.dll is loaded at runtime so no import library is required.
#![allow(non_camel_case_types)]
use anyhow::{Result, anyhow};
use libloading::Library;
use parking_lot::{Condvar, Mutex};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

type mpv_handle = c_void;
type mpv_render_context = c_void;

#[repr(C)]
struct mpv_render_param {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct mpv_event {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct mpv_event_property {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct mpv_event_end_file {
    reason: c_int,
    error: c_int,
}

const MPV_FORMAT_STRING: c_int = 1;
const MPV_FORMAT_FLAG: c_int = 3;
const MPV_FORMAT_DOUBLE: c_int = 5;

const EV_SHUTDOWN: c_int = 1;
const EV_END_FILE: c_int = 7;
const EV_FILE_LOADED: c_int = 8;
const EV_PROPERTY_CHANGE: c_int = 22;
const EV_PLAYBACK_RESTART: c_int = 21;

const RP_API_TYPE: c_int = 1;
const RP_BLOCK_FOR_TARGET_TIME: c_int = 12;
const RP_SW_SIZE: c_int = 17;
const RP_SW_FORMAT: c_int = 18;
const RP_SW_STRIDE: c_int = 19;
const RP_SW_POINTER: c_int = 20;
const RENDER_UPDATE_FRAME: u64 = 1;

struct Fns {
    create: unsafe extern "C" fn() -> *mut mpv_handle,
    initialize: unsafe extern "C" fn(*mut mpv_handle) -> c_int,
    terminate_destroy: unsafe extern "C" fn(*mut mpv_handle),
    set_option_string: unsafe extern "C" fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int,
    set_property_string: unsafe extern "C" fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int,
    set_property: unsafe extern "C" fn(*mut mpv_handle, *const c_char, c_int, *mut c_void) -> c_int,
    get_property: unsafe extern "C" fn(*mut mpv_handle, *const c_char, c_int, *mut c_void) -> c_int,
    command: unsafe extern "C" fn(*mut mpv_handle, *mut *const c_char) -> c_int,
    command_async: unsafe extern "C" fn(*mut mpv_handle, u64, *mut *const c_char) -> c_int,
    observe_property: unsafe extern "C" fn(*mut mpv_handle, u64, *const c_char, c_int) -> c_int,
    wait_event: unsafe extern "C" fn(*mut mpv_handle, f64) -> *mut mpv_event,
    wakeup: unsafe extern "C" fn(*mut mpv_handle),
    free: unsafe extern "C" fn(*mut c_void),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    rc_create: unsafe extern "C" fn(*mut *mut mpv_render_context, *mut mpv_handle, *mut mpv_render_param) -> c_int,
    rc_set_update_callback:
        unsafe extern "C" fn(*mut mpv_render_context, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void),
    rc_update: unsafe extern "C" fn(*mut mpv_render_context) -> u64,
    rc_render: unsafe extern "C" fn(*mut mpv_render_context, *mut mpv_render_param) -> c_int,
    rc_free: unsafe extern "C" fn(*mut mpv_render_context),
}

fn lib() -> Result<&'static (Library, Fns)> {
    static LIB: std::sync::OnceLock<Result<(Library, Fns), String>> = std::sync::OnceLock::new();
    LIB.get_or_init(|| unsafe {
        #[cfg(windows)]
        preload_vulkan_loader();
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(d) = exe.parent() {
                candidates.push(d.join("libmpv-2.dll"));
                candidates.push(d.join("mpv-2.dll"));
                // the AppImage carries its own libmpv in usr/lib, next to usr/bin
                #[cfg(target_os = "linux")]
                for name in ["libmpv.so.2", "libmpv.so.1"] {
                    let p = d.join("../lib").join(name);
                    if p.is_file() {
                        candidates.push(p);
                    }
                }
            }
        }
        #[cfg(windows)]
        candidates.push(std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/third_party/mpv/libmpv-2.dll")));
        #[cfg(not(windows))]
        {
            candidates.push("libmpv.so.2".into());
            candidates.push("libmpv.so.1".into());
            candidates.push("libmpv.so".into());
            candidates.push("libmpv.2.dylib".into());
        }
        let mut last_err = String::from("libmpv not found");
        // the error from a copy that is there but won't load says more than
        // "not found" from the names tried after it
        let mut first_real_err: Option<String> = None;
        for c in candidates {
            #[cfg(windows)]
            if !c.is_absolute() || !c.is_file() { continue; }
            #[cfg(windows)]
            let loaded = libloading::os::windows::Library::load_with_flags(&c, 0x1100).map(Library::from);
            #[cfg(not(windows))]
            let loaded = Library::new(&c);
            match loaded {
                Ok(l) => {
                    macro_rules! sym {
                        ($n:literal) => {
                            *l.get(concat!($n, "\0").as_bytes()).map_err(|e| e.to_string())?
                        };
                    }
                    let fns = Fns {
                        create: sym!("mpv_create"),
                        initialize: sym!("mpv_initialize"),
                        terminate_destroy: sym!("mpv_terminate_destroy"),
                        set_option_string: sym!("mpv_set_option_string"),
                        set_property_string: sym!("mpv_set_property_string"),
                        set_property: sym!("mpv_set_property"),
                        get_property: sym!("mpv_get_property"),
                        command: sym!("mpv_command"),
                        command_async: sym!("mpv_command_async"),
                        observe_property: sym!("mpv_observe_property"),
                        wait_event: sym!("mpv_wait_event"),
                        wakeup: sym!("mpv_wakeup"),
                        free: sym!("mpv_free"),
                        error_string: sym!("mpv_error_string"),
                        rc_create: sym!("mpv_render_context_create"),
                        rc_set_update_callback: sym!("mpv_render_context_set_update_callback"),
                        rc_update: sym!("mpv_render_context_update"),
                        rc_render: sym!("mpv_render_context_render"),
                        rc_free: sym!("mpv_render_context_free"),
                    };
                    return Ok((l, fns));
                }
                Err(e) => {
                    last_err = format!("{}: {e}", c.display());
                    if first_real_err.is_none() && c.is_absolute() && c.is_file() {
                        first_real_err = Some(last_err.clone());
                    }
                }
            }
        }
        Err(first_real_err.unwrap_or(last_err))
    })
    .as_ref()
    .map_err(|e| anyhow!("{e}"))
}

/// libmpv-2.dll links against vulkan-1.dll, which Windows only has when the GPU
/// driver installed it (not with older Intel graphics such as HD 4000, or in
/// some VMs). Without it libmpv fails to load at all, even though playback here
/// never uses Vulkan. The app ships the Khronos loader in a sub-folder and loads
/// it only when the system has none; once a module named vulkan-1.dll is loaded,
/// Windows uses it to satisfy libmpv's import.
#[cfg(windows)]
unsafe fn preload_vulkan_loader() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    // system copy (System32), never one from the current directory
    let sys = std::env::var_os("SystemRoot")
        .map(|r| std::path::PathBuf::from(r).join("System32").join("vulkan-1.dll"))
        .filter(|p| p.is_file());
    if sys.is_some() {
        return;
    }
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) else { return };
    for p in [dir.join("vulkan").join("vulkan-1.dll"), std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/third_party/vulkan/vulkan-1.dll"))] {
        if p.is_file() {
            if let Ok(l) = unsafe { libloading::os::windows::Library::load_with_flags(&p, 0x1100) } {
                // stays loaded for the life of the process
                std::mem::forget(l);
                return;
            }
        }
    }
}

pub fn available() -> bool {
    lib().is_ok()
}

#[derive(Clone, Debug)]
pub enum MpvEvent {
    FileLoaded,
    PlaybackRestart,
    EndFile { eof: bool, error: Option<String> },
    Prop(&'static str, PropValue),
    Frame,
    Shutdown,
}

#[derive(Clone, Debug)]
pub enum PropValue {
    Double(f64),
    Flag(bool),
    Str(String),
}

const OBSERVED: &[(&str, c_int)] = &[
    ("time-pos", MPV_FORMAT_DOUBLE),
    ("duration", MPV_FORMAT_DOUBLE),
    ("pause", MPV_FORMAT_FLAG),
    ("eof-reached", MPV_FORMAT_FLAG),
    ("paused-for-cache", MPV_FORMAT_FLAG),
    ("seeking", MPV_FORMAT_FLAG),
    ("demuxer-cache-time", MPV_FORMAT_DOUBLE),
    ("speed", MPV_FORMAT_DOUBLE),
    ("volume", MPV_FORMAT_DOUBLE),
    ("mute", MPV_FORMAT_FLAG),
    ("dwidth", MPV_FORMAT_DOUBLE),
    ("dheight", MPV_FORMAT_DOUBLE),
    ("core-idle", MPV_FORMAT_FLAG),
];

struct Handle(*mut mpv_handle);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

struct RenderCtx(*mut mpv_render_context);
unsafe impl Send for RenderCtx {}

/// A rendered video frame (BGRA, alpha forced to 255).
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub serial: u64,
}

struct RenderShared {
    wake: Mutex<bool>,
    cv: Condvar,
    target: Mutex<(u32, u32)>,
    frame: Mutex<Option<VideoFrame>>,
    serial: AtomicU64,
    stop: AtomicBool,
    force: AtomicBool,
}

pub struct Mpv {
    h: Arc<Handle>,
    render: Arc<RenderShared>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

unsafe extern "C" fn on_render_update(ctx: *mut c_void) {
    let shared = unsafe { &*(ctx as *const RenderShared) };
    let mut w = shared.wake.lock();
    *w = true;
    shared.cv.notify_one();
}

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Block([u8; 64]);

impl Mpv {
    /// Creates a player. `events` receives property changes and frame notifications.
    pub fn new(options: &[(&str, &str)], events: impl Fn(MpvEvent) + Send + Sync + 'static) -> Result<Arc<Self>> {
        let (_, f) = lib()?;
        let events = Arc::new(events);
        unsafe {
            let h = (f.create)();
            if h.is_null() {
                return Err(anyhow!("mpv_create failed"));
            }
            for (k, v) in options {
                let k = CString::new(*k)?;
                let v = CString::new(*v)?;
                (f.set_option_string)(h, k.as_ptr(), v.as_ptr());
            }
            let rc = (f.initialize)(h);
            if rc < 0 {
                return Err(anyhow!("mpv_initialize failed: {}", err_str(rc)));
            }
            for (i, (name, fmt)) in OBSERVED.iter().enumerate() {
                let n = CString::new(*name)?;
                (f.observe_property)(h, i as u64, n.as_ptr(), *fmt);
            }
            let render = Arc::new(RenderShared {
                wake: Mutex::new(false),
                cv: Condvar::new(),
                target: Mutex::new((640, 360)),
                frame: Mutex::new(None),
                serial: AtomicU64::new(0),
                stop: AtomicBool::new(false),
                force: AtomicBool::new(false),
            });
            // render context
            let api = CString::new("sw")?;
            let mut params = [
                mpv_render_param { kind: RP_API_TYPE, data: api.as_ptr() as *mut c_void },
                mpv_render_param { kind: 0, data: std::ptr::null_mut() },
            ];
            let mut rctx: *mut mpv_render_context = std::ptr::null_mut();
            let rc = (f.rc_create)(&mut rctx, h, params.as_mut_ptr());
            if rc < 0 {
                (f.terminate_destroy)(h);
                return Err(anyhow!("render context: {}", err_str(rc)));
            }
            (f.rc_set_update_callback)(rctx, Some(on_render_update), Arc::as_ptr(&render) as *mut c_void);

            let handle = Arc::new(Handle(h));
            let mpv = Arc::new(Mpv { h: handle.clone(), render: render.clone(), threads: Mutex::new(Vec::new()) });

            // event thread
            let ev_handle = handle.clone();
            let ev_cb = events.clone();
            let t1 = std::thread::Builder::new().name("mpv-events".into()).spawn(move || {
                let (_, f) = lib().unwrap();
                loop {
                    let e = (f.wait_event)(ev_handle.0, -1.0);
                    if e.is_null() {
                        continue;
                    }
                    let e = &*e;
                    match e.event_id {
                        0 => {}
                        EV_SHUTDOWN => {
                            ev_cb(MpvEvent::Shutdown);
                            break;
                        }
                        EV_FILE_LOADED => ev_cb(MpvEvent::FileLoaded),
                        EV_PLAYBACK_RESTART => ev_cb(MpvEvent::PlaybackRestart),
                        EV_END_FILE => {
                            let d = &*(e.data as *const mpv_event_end_file);
                            // reason: 0 eof, 2 stop, 3 quit, 4 error, 5 redirect
                            ev_cb(MpvEvent::EndFile {
                                eof: d.reason == 0,
                                error: (d.reason == 4).then(|| err_str(d.error)),
                            });
                        }
                        EV_PROPERTY_CHANGE => {
                            let p = &*(e.data as *const mpv_event_property);
                            let idx = e.reply_userdata as usize;
                            if idx < OBSERVED.len() && !p.data.is_null() {
                                let name = OBSERVED[idx].0;
                                let val = match p.format {
                                    MPV_FORMAT_DOUBLE => Some(PropValue::Double(*(p.data as *const f64))),
                                    MPV_FORMAT_FLAG => Some(PropValue::Flag(*(p.data as *const c_int) != 0)),
                                    MPV_FORMAT_STRING => {
                                        let s = *(p.data as *const *const c_char);
                                        (!s.is_null()).then(|| PropValue::Str(CStr::from_ptr(s).to_string_lossy().into_owned()))
                                    }
                                    _ => None,
                                };
                                if let Some(v) = val {
                                    ev_cb(MpvEvent::Prop(name, v));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            })?;

            // render thread
            let r = render.clone();
            let frame_cb = events.clone();
            let rc_ptr = RenderCtx(rctx);
            let t2 = std::thread::Builder::new().name("mpv-render".into()).spawn(move || {
                let rc_ptr = rc_ptr;
                let (_, f) = lib().unwrap();
                let mut buf: Vec<Block> = Vec::new();
                let mut last_size = (0u32, 0u32);
                let fmt = CString::new("bgr0").unwrap();
                loop {
                    {
                        let mut w = r.wake.lock();
                        while !*w && !r.stop.load(Ordering::Relaxed) {
                            r.cv.wait(&mut w);
                        }
                        *w = false;
                    }
                    if r.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let flags = (f.rc_update)(rc_ptr.0);
                    let forced = r.force.swap(false, Ordering::Relaxed);
                    if flags & RENDER_UPDATE_FRAME == 0 && !forced {
                        continue;
                    }
                    let (tw, th) = *r.target.lock();
                    let w = ((tw.max(16) + 15) / 16) * 16;
                    let h = th.max(16);
                    let stride = w as usize * 4;
                    let bytes = stride * h as usize;
                    let blocks = bytes.div_ceil(64);
                    if (w, h) != last_size {
                        // a new size reinterprets the old pixels at a different stride, which
                        // shows up as streaks until mpv has a frame to draw: start from black
                        last_size = (w, h);
                        buf.clear();
                        buf.resize(blocks, Block([0; 64]));
                    }
                    let mut size = [w as c_int, h as c_int];
                    let mut stride_v: usize = stride;
                    let mut no_block: c_int = 0;
                    let mut params = [
                        mpv_render_param { kind: RP_SW_SIZE, data: size.as_mut_ptr() as *mut c_void },
                        mpv_render_param { kind: RP_SW_FORMAT, data: fmt.as_ptr() as *mut c_void },
                        mpv_render_param { kind: RP_SW_STRIDE, data: &mut stride_v as *mut usize as *mut c_void },
                        mpv_render_param { kind: RP_SW_POINTER, data: buf.as_mut_ptr() as *mut c_void },
                        mpv_render_param { kind: RP_BLOCK_FOR_TARGET_TIME, data: &mut no_block as *mut c_int as *mut c_void },
                        mpv_render_param { kind: 0, data: std::ptr::null_mut() },
                    ];
                    let t0 = std::time::Instant::now();
                    let rc = (f.rc_render)(rc_ptr.0, params.as_mut_ptr());
                    crate::perf::add(&crate::perf::MPV_RENDER_US, t0.elapsed().as_micros() as u64);
                    crate::perf::add(&crate::perf::MPV_FRAMES, 1);
                    if rc < 0 {
                        continue;
                    }
                    // one pass: copy each pixel out with its padding byte set to opaque
                    // (the buffer is 64-byte aligned, so reading it as u32 is fine)
                    let src = std::slice::from_raw_parts(buf.as_ptr() as *const u32, bytes / 4);
                    let opaque = u32::from_ne_bytes([0, 0, 0, 0xFF]);
                    let mut data = vec![0u8; bytes];
                    for (d, s) in data.chunks_exact_mut(4).zip(src) {
                        d.copy_from_slice(&(s | opaque).to_ne_bytes());
                    }
                    let serial = r.serial.fetch_add(1, Ordering::Relaxed) + 1;
                    *r.frame.lock() = Some(VideoFrame { width: w, height: h, data, serial });
                    frame_cb(MpvEvent::Frame);
                }
                (f.rc_free)(rc_ptr.0);
            })?;
            mpv.threads.lock().extend([t1, t2]);
            Ok(mpv)
        }
    }

    /// Sets the size frames are rendered at (physical pixels).
    pub fn set_render_size(&self, w: u32, h: u32) {
        let mut t = self.render.target.lock();
        let (w, h) = (w.clamp(16, 3840), h.clamp(16, 2160));
        if *t != (w, h) {
            *t = (w, h);
            drop(t);
            self.redraw();
        }
    }

    /// Forces a re-render of the current frame (e.g. after resize while paused).
    pub fn redraw(&self) {
        self.render.force.store(true, Ordering::Relaxed);
        let mut w = self.render.wake.lock();
        *w = true;
        self.render.cv.notify_one();
    }

    pub fn take_frame(&self) -> Option<VideoFrame> {
        self.render.frame.lock().take()
    }

    pub fn command(&self, args: &[&str]) -> Result<()> {
        let (_, f) = lib()?;
        let cs: Vec<CString> = args.iter().map(|a| CString::new(*a).unwrap_or_default()).collect();
        let mut ptrs: Vec<*const c_char> = cs.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let rc = unsafe { (f.command_async)(self.h.0, 0, ptrs.as_mut_ptr()) };
        if rc < 0 { Err(anyhow!("mpv command {:?}: {}", args.first(), err_str(rc))) } else { Ok(()) }
    }

    pub fn command_sync(&self, args: &[&str]) -> Result<()> {
        let (_, f) = lib()?;
        let cs: Vec<CString> = args.iter().map(|a| CString::new(*a).unwrap_or_default()).collect();
        let mut ptrs: Vec<*const c_char> = cs.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let rc = unsafe { (f.command)(self.h.0, ptrs.as_mut_ptr()) };
        if rc < 0 { Err(anyhow!("mpv command {:?}: {}", args.first(), err_str(rc))) } else { Ok(()) }
    }

    pub fn set(&self, name: &str, value: &str) {
        if let Ok((_, f)) = lib() {
            let n = CString::new(name).unwrap_or_default();
            let v = CString::new(value).unwrap_or_default();
            unsafe {
                (f.set_property_string)(self.h.0, n.as_ptr(), v.as_ptr());
            }
        }
    }

    pub fn set_f64(&self, name: &str, mut value: f64) {
        if let Ok((_, f)) = lib() {
            let n = CString::new(name).unwrap_or_default();
            unsafe {
                (f.set_property)(self.h.0, n.as_ptr(), MPV_FORMAT_DOUBLE, &mut value as *mut f64 as *mut c_void);
            }
        }
    }

    pub fn set_flag(&self, name: &str, value: bool) {
        if let Ok((_, f)) = lib() {
            let n = CString::new(name).unwrap_or_default();
            let mut v: c_int = value as c_int;
            unsafe {
                (f.set_property)(self.h.0, n.as_ptr(), MPV_FORMAT_FLAG, &mut v as *mut c_int as *mut c_void);
            }
        }
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        let (_, f) = lib().ok()?;
        let n = CString::new(name).ok()?;
        let mut v: f64 = 0.0;
        let rc = unsafe { (f.get_property)(self.h.0, n.as_ptr(), MPV_FORMAT_DOUBLE, &mut v as *mut f64 as *mut c_void) };
        (rc >= 0).then_some(v)
    }

    pub fn get_string(&self, name: &str) -> Option<String> {
        let (_, f) = lib().ok()?;
        let n = CString::new(name).ok()?;
        let mut p: *mut c_char = std::ptr::null_mut();
        let rc = unsafe { (f.get_property)(self.h.0, n.as_ptr(), MPV_FORMAT_STRING, &mut p as *mut *mut c_char as *mut c_void) };
        if rc < 0 || p.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() };
        unsafe { (f.free)(p as *mut c_void) };
        Some(s)
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        self.render.stop.store(true, Ordering::Relaxed);
        {
            let mut w = self.render.wake.lock();
            *w = true;
            self.render.cv.notify_all();
        }
        let threads: Vec<_> = self.threads.lock().drain(..).collect();
        let mut it = threads.into_iter();
        let ev = it.next();
        // 1) join the render thread (it frees the render context)
        for t in it {
            let _ = t.join();
        }
        // 2) ask the core to quit; the event thread exits on SHUTDOWN
        let _ = self.command_sync(&["quit"]);
        if let Some(t) = ev {
            let _ = t.join();
        }
        // 3) destroy once nobody is waiting on the handle
        if let Ok((_, f)) = lib() {
            unsafe {
                (f.terminate_destroy)(self.h.0);
            }
        }
    }
}

fn err_str(rc: c_int) -> String {
    lib()
        .ok()
        .map(|(_, f)| unsafe { CStr::from_ptr((f.error_string)(rc)).to_string_lossy().into_owned() })
        .unwrap_or_else(|| rc.to_string())
}
