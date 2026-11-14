//! one owner per data folder. the first ryad holds a named mutex and an
//! exclusive lock file in the session folder; any later start hands its
//! (already absolute) paths to the owner's window with WM_COPYDATA and exits.
//!
//! races covered: two starts at once (the mutex picks one), a start while
//! the owner is still creating its window (retry until the window answers),
//! and a start while the owner is closing (it refuses new paths; the
//! mutex disappears with the process and the newcomer becomes the owner).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_COPYDATA,
};
use windows::core::PCWSTR;

use crate::files::fnv64;
use crate::wide;

/// marks our WM_COPYDATA among anything else a window may receive.
pub const COPYDATA_PATHS: usize = 0x7279_6164; // "ryad"

pub struct Owner {
    mutex: HANDLE,
    _lock: std::fs::File,
    /// window class of this owner (unique per data folder)
    pub class: Vec<u16>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: our own mutex handle, closed once.
        unsafe {
            let _ = CloseHandle(self.mutex);
        }
    }
}

pub enum Claim {
    Owner(Owner),
    Handed,
    Failed(String),
}

pub fn class_for(data: &Path) -> Vec<u16> {
    wide(&format!("ryad.main.{:016x}", key(data)))
}

fn key(data: &Path) -> u64 {
    fnv64(data.to_string_lossy().to_lowercase().as_bytes())
}

pub fn claim(data: &Path, paths: &[PathBuf]) -> Claim {
    let name = wide(&format!("Local\\ryad.{:016x}", key(data)));
    let class = class_for(data);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // SAFETY: named mutex; the handle is closed in Owner::drop or below.
        let mutex = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) };
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        match mutex {
            Ok(m) if !existed => match lock(data) {
                Ok(file) => return Claim::Owner(Owner { mutex: m, _lock: file, class }),
                Err(e) => {
                    // the mutex is ours but the lock file is still held: a
                    // previous owner is finishing. wait for it.
                    // SAFETY: our own handle.
                    unsafe {
                        let _ = CloseHandle(m);
                    }
                    if Instant::now() > deadline {
                        return Claim::Failed(format!("ryad could not open its session folder: {e}"));
                    }
                }
            },
            Ok(m) => {
                // SAFETY: our handle to someone else's mutex; not needed.
                unsafe {
                    let _ = CloseHandle(m);
                }
                // SAFETY: plain lookup by class name.
                if let Ok(w) = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }
                    && hand(w, paths)
                {
                    return Claim::Handed;
                }
            }
            Err(e) => return Claim::Failed(format!("ryad could not check for a running copy ({})", e.message())),
        }
        if Instant::now() > deadline {
            return Claim::Failed("ryad is still starting or closing in another window. try again in a moment.".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn lock(data: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = data.join("session");
    std::fs::create_dir_all(&dir)?;
    std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).open(dir.join("lock"))
}

/// paths as utf-16, separated and ended by a zero unit.
pub fn encode_paths(paths: &[PathBuf]) -> Vec<u16> {
    let mut v = Vec::new();
    for p in paths {
        v.extend(p.as_os_str().to_string_lossy().encode_utf16());
        v.push(0);
    }
    v
}

pub fn decode_paths(units: &[u16]) -> Vec<PathBuf> {
    units.split(|&u| u == 0).filter(|s| !s.is_empty()).map(|s| PathBuf::from(String::from_utf16_lossy(s))).collect()
}

fn hand(w: HWND, paths: &[PathBuf]) -> bool {
    let data = encode_paths(paths);
    let cds = COPYDATASTRUCT { dwData: COPYDATA_PATHS, cbData: (data.len() * 2) as u32, lpData: data.as_ptr() as *mut _ };
    let mut result = 0usize;
    // SAFETY: the struct and buffer outlive the synchronous call; the owner
    // may bring itself to the front because we allow it here.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(w, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        let ok = SendMessageTimeoutW(w, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, 5000, Some(&mut result));
        ok.0 != 0 && result == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_round_trip() {
        let p = vec![PathBuf::from("C:\\з пробілом\\файл.txt"), PathBuf::from("D:\\без розширення")];
        assert_eq!(decode_paths(&encode_paths(&p)), p);
        assert!(decode_paths(&encode_paths(&[])).is_empty());
    }
}
