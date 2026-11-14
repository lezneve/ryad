//! ryad: a small windows editor that keeps your drafts between launches.
//!
//!   ryad [path...]              open files (in the running ryad, if there is one)
//!   ryad --data <folder> ...    use another data folder (tests); RYAD_DATA does the same
//!   ryad --trace ...            print timings to stderr (never text contents)
//!   ryad --install / --uninstall, or a copy named ryad-<version>-setup.exe
//!   ryad --finish-uninstall <folder> --wait <pid>   (the temp copy that removes the program folder)
//!
//! one process owns the session and the window. a second start only hands
//! its paths to that process and exits.

#![windows_subsystem = "windows"]

mod app;
mod canvas;
mod dialogs;
mod document;
mod editor;
mod files;
mod find;
mod install;
mod installer;
mod ipc;
mod markdown;
mod menubar;
mod preview;
mod session;
mod settings;
mod tabs;
mod ui;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

/// the product name; the only place it is spelled out in code.
pub const APP_NAME: &str = "ryad";

static TRACE: AtomicBool = AtomicBool::new(false);

/// timing/diagnostic line: durations, sizes, error codes, revisions. never
/// text, search terms or clipboard contents.
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        if $crate::tracing() {
            eprintln!("[ryad {:>8.1}] {}", $crate::ms_since_process_start(), format!($($arg)*));
        }
    };
}

pub fn tracing() -> bool {
    TRACE.load(Ordering::Relaxed)
}

/// null-terminated utf-16 for win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct Args {
    pub data: PathBuf,
    pub paths: Vec<PathBuf>,
    install: bool,
    uninstall: bool,
    finish_uninstall: Option<PathBuf>,
    wait_pid: Option<u32>,
}

fn parse_args() -> Result<Args, String> {
    let mut data = std::env::var_os("RYAD_DATA").map(PathBuf::from);
    let mut paths = Vec::new();
    let (mut install, mut uninstall) = (false, false);
    let (mut finish_uninstall, mut wait_pid) = (None, None);
    let mut it = std::env::args_os().skip(1);
    let mut only_paths = false;
    while let Some(arg) = it.next() {
        let s = arg.to_string_lossy();
        if !only_paths && s.starts_with("--") {
            match s.as_ref() {
                "--" => only_paths = true,
                "--trace" => TRACE.store(true, Ordering::Relaxed),
                "--data" => data = Some(it.next().ok_or("--data needs a folder")?.into()),
                "--install" => install = true,
                "--uninstall" => uninstall = true,
                "--finish-uninstall" => finish_uninstall = Some(it.next().ok_or("--finish-uninstall needs a folder")?.into()),
                "--wait" => wait_pid = it.next().and_then(|v| v.to_string_lossy().parse().ok()),
                other => return Err(format!("unknown option: {other}")),
            }
            continue;
        }
        // relative paths are resolved here, in the folder ryad was started
        // from, before they are handed to another process.
        let p = PathBuf::from(&arg);
        paths.push(std::path::absolute(&p).unwrap_or(p));
    }
    let data = match data {
        Some(d) => std::path::absolute(&d).unwrap_or(d),
        None => default_data_dir()?,
    };
    Ok(Args { data, paths, install, uninstall, finish_uninstall, wait_pid })
}

/// `%LOCALAPPDATA%\ryad`
pub fn default_data_dir() -> Result<PathBuf, String> {
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: the returned string is freed with CoTaskMemFree after copying.
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None)
            .map_err(|e| format!("windows did not give ryad its local app data folder ({})", e.message()))?;
        let s = p.to_string().unwrap_or_default();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        Ok(PathBuf::from(s).join(APP_NAME))
    }
}

fn main() {
    // the manifest already declares per-monitor-v2; this is a fallback.
    // SAFETY: process-wide setting with a constant argument.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            dialogs::error(None, &e);
            std::process::exit(2);
        }
    };
    if tracing() {
        // a gui-subsystem exe has no console; borrow the parent's unless the
        // output was redirected to a file.
        // SAFETY: handle queries and a constant argument.
        unsafe {
            let redirected = |which| GetStdHandle(which).is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
            if !redirected(STD_ERROR_HANDLE) && !redirected(STD_OUTPUT_HANDLE) {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }
    install_panic_hook(&args.data);

    let setup_name =
        std::env::current_exe().ok().and_then(|e| e.file_stem().map(|s| s.to_string_lossy().into_owned())).is_some_and(|s| install::is_setup_name(&s));

    // ole (not just com): the rich edit control's drag and drop and the
    // system file dialogs need it on the ui thread.
    // SAFETY: paired with OleUninitialize below.
    if unsafe { OleInitialize(None) }.is_err() {
        dialogs::error(None, "ryad could not start: windows did not let it set up ole on this thread. try again, or restart windows.");
        std::process::exit(1);
    }
    let code = if let Some(dir) = &args.finish_uninstall {
        install::finish_uninstall(dir, args.wait_pid)
    } else if args.uninstall {
        install::uninstall_interactive()
    } else if args.install || (setup_name && !install::running_installed()) {
        install::install_interactive()
    } else {
        run(args)
    };
    // SAFETY: matches the successful OleInitialize.
    unsafe { OleUninitialize() };
    std::process::exit(code);
}

fn run(args: Args) -> i32 {
    trace!("start, data folder set");
    match ipc::claim(&args.data, &args.paths) {
        ipc::Claim::Handed => {
            trace!("paths handed to the running ryad");
            0
        }
        ipc::Claim::Failed(e) => {
            dialogs::error(None, &e);
            1
        }
        ipc::Claim::Owner(owner) => app::run(args.data, args.paths, owner),
    }
}

/// a panic ends the process (panic = "abort"); the session keeps the last
/// committed copy either way. the hook only leaves a line for diagnosis.
fn install_panic_hook(data: &std::path::Path) {
    let log = data.join("crash.log");
    std::panic::set_hook(Box::new(move |info| {
        let place = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let line = format!("{} panic at {place}\n", unix_time());
        eprint!("[ryad] {line}");
        let _ = std::fs::create_dir_all(log.parent().unwrap_or(std::path::Path::new(".")));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
    }));
}

pub fn unix_time() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// milliseconds since this process was created (includes loader time).
pub fn ms_since_process_start() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: out-pointers to locals; pseudo-handle of the current process.
    unsafe {
        if GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user).is_err() {
            return f64::NAN;
        }
        let now = GetSystemTimePreciseAsFileTime();
        let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        (t(now).saturating_sub(t(created))) as f64 / 10_000.0
    }
}

/// private bytes and working set of this process, for trace output.
pub fn memory_summary() -> String {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    // SAFETY: out-pointer to a local of the size passed.
    if unsafe { GetProcessMemoryInfo(GetCurrentProcess(), (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(), c.cb) }.is_err() {
        return "unknown".into();
    }
    let mib = |b: usize| b as f64 / 1048576.0;
    format!("ws {:.1} MiB, private {:.1} MiB (peak {:.1})", mib(c.WorkingSetSize), mib(c.PrivateUsage), mib(c.PeakPagefileUsage))
}
