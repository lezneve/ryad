//! install and uninstall for the current user, without admin rights. the
//! installer is ryad.exe itself under a setup name (or `ryad --install`):
//!
//!   %LOCALAPPDATA%\Programs\ryad\ryad.exe
//!   start menu   %APPDATA%\Microsoft\Windows\Start Menu\Programs\ryad.lnk
//!   apps entry   HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\ryad
//!   open with    HKCU\Software\Classes\ryad.text (progid), Applications\ryad.exe,
//!                <ext>\OpenWithProgids, Software\ryad\Capabilities,
//!                Software\RegisteredApplications
//!
//! ryad never makes itself the default app: windows asks the user for that.
//! drafts and settings (%LOCALAPPDATA%\ryad) are left alone by both install
//! and uninstall, so a later install finds the session again.
//!
//! structure and the self-removing uninstall come from kadr (src/install.rs
//! at 29db81d, MIT). test-only: RYAD_TEST_SANDBOX=<folder> puts every file
//! under that folder and every registry key under HKCU\Software\ryad-test.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW,
    RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, SHGetKnownFolderPath, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE};
use windows::core::{Interface, PCWSTR, w};

use crate::{APP_NAME, trace, wide};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const PROGID: &str = "ryad.text";
/// the types ryad offers itself for in "open with". any other file opens
/// through the dialog, drag and drop or the command line.
pub const EXTENSIONS: [&str; 8] = [".txt", ".log", ".md", ".ini", ".cfg", ".json", ".csv", ".xml"];

/// the installer runs only for a file named like a release download:
/// `ryad-setup` or `ryad-<version>-setup`, optionally with the " (2)" a
/// browser adds. same rule as kadr.
pub fn is_setup_name(stem: &str) -> bool {
    let mut s = stem.trim().to_ascii_lowercase();
    if let Some(open) = s.rfind(" (")
        && s.ends_with(')')
        && s.len() > open + 3
        && s[open + 2..s.len() - 1].chars().all(|c| c.is_ascii_digit())
    {
        s.truncate(open);
    }
    if s == "ryad-setup" || s == "ryad_setup" {
        return true;
    }
    let Some(middle) = s.strip_prefix("ryad-").and_then(|r| r.strip_suffix("-setup")) else { return false };
    middle.starts_with(|c: char| c.is_ascii_digit()) && middle.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn sandbox() -> Option<PathBuf> {
    std::env::var_os("RYAD_TEST_SANDBOX").map(PathBuf::from)
}

pub struct Paths {
    pub dir: PathBuf,
    pub exe: PathBuf,
    pub shortcut: PathBuf,
    pub data: PathBuf,
    /// where every registry path below lives: "Software" or "Software\ryad-test"
    reg_root: String,
}

fn known(id: &windows::core::GUID) -> PathBuf {
    // SAFETY: the returned string is freed after copying.
    unsafe {
        match SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
                PathBuf::from(s)
            }
            Err(_) => PathBuf::new(),
        }
    }
}

pub fn paths() -> Paths {
    match sandbox() {
        Some(root) => Paths {
            dir: root.join("Programs").join(APP_NAME),
            exe: root.join("Programs").join(APP_NAME).join(format!("{APP_NAME}.exe")),
            shortcut: root.join("StartMenu").join(format!("{APP_NAME}.lnk")),
            data: std::env::var_os("RYAD_DATA").map(PathBuf::from).unwrap_or_else(|| root.join("data")),
            reg_root: "Software\\ryad-test".into(),
        },
        None => {
            let dir = known(&FOLDERID_LocalAppData).join("Programs").join(APP_NAME);
            Paths {
                exe: dir.join(format!("{APP_NAME}.exe")),
                dir,
                shortcut: known(&FOLDERID_Programs).join(format!("{APP_NAME}.lnk")),
                data: std::env::var_os("RYAD_DATA").map(PathBuf::from).or_else(|| crate::default_data_dir().ok()).unwrap_or_default(),
                reg_root: "Software".into(),
            }
        }
    }
}

impl Paths {
    fn reg(&self, rel: &str) -> String {
        format!("{}\\{rel}", self.reg_root)
    }
    fn uninstall_key(&self) -> String {
        self.reg(&format!("Microsoft\\Windows\\CurrentVersion\\Uninstall\\{APP_NAME}"))
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

pub fn running_installed() -> bool {
    std::env::current_exe().ok().is_some_and(|e| same_file(&e, &paths().exe))
}

// ---- registry ------------------------------------------------------------------

fn set_value(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    let k = wide(key);
    let mut h = HKEY::default();
    // SAFETY: the key is created for writing and closed below; strings live
    // through the calls.
    unsafe {
        RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut h, None)
            .ok()
            .map_err(|e| format!("{key}: {}", e.message()))?;
        let n = name.map(wide);
        let v = wide(value);
        let bytes = std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), v.len() * 2);
        let r = RegSetValueExW(h, n.as_ref().map(|n| PCWSTR(n.as_ptr())).unwrap_or(PCWSTR::null()), None, REG_SZ, Some(bytes)).ok();
        let _ = RegCloseKey(h);
        r.map_err(|e| format!("{key}: {}", e.message()))
    }
}

fn set_dword(key: &str, name: &str, value: u32) {
    let k = wide(key);
    let mut h = HKEY::default();
    // SAFETY: as above.
    unsafe {
        if RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut h, None).is_ok() {
            let n = wide(name);
            let _ = RegSetValueExW(h, PCWSTR(n.as_ptr()), None, REG_DWORD, Some(&value.to_le_bytes()));
            let _ = RegCloseKey(h);
        }
    }
}

fn get_value(key: &str, name: &str) -> Option<String> {
    let k = wide(key);
    let n = wide(name);
    let mut buf = vec![0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: out-pointers to a local buffer of the size passed alongside.
    let status =
        unsafe { RegGetValueW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), PCWSTR(n.as_ptr()), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
    status.is_ok().then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
}

fn delete_tree(key: &str) {
    let k = wide(key);
    // SAFETY: deletes our own key; a missing key is fine.
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()));
        let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()));
    }
}

fn delete_value(key: &str, name: &str) {
    let (k, n) = (wide(key), wide(name));
    // SAFETY: deletes one value of a key; missing is fine.
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), PCWSTR(n.as_ptr()));
    }
}

pub fn installed_version() -> Option<String> {
    get_value(&paths().uninstall_key(), "DisplayVersion")
}

/// "open with" registration: a progid, the application entry with the
/// supported types, the progid under each type's OpenWithProgids, and the
/// capabilities for "default apps". no default is taken over.
fn register(p: &Paths) -> Result<(), String> {
    let exe = p.exe.display().to_string();
    let open = format!("\"{exe}\" \"%1\"");
    let classes = p.reg("Classes");
    set_value(&format!("{classes}\\{PROGID}"), None, "text document")?;
    set_value(&format!("{classes}\\{PROGID}"), Some("FriendlyTypeName"), "text document")?;
    set_value(&format!("{classes}\\{PROGID}\\DefaultIcon"), None, &format!("{exe},0"))?;
    set_value(&format!("{classes}\\{PROGID}\\shell\\open\\command"), None, &open)?;
    let app = format!("{classes}\\Applications\\{APP_NAME}.exe");
    set_value(&app, Some("FriendlyAppName"), APP_NAME)?;
    set_value(&format!("{app}\\DefaultIcon"), None, &format!("{exe},0"))?;
    set_value(&format!("{app}\\shell\\open\\command"), None, &open)?;
    let caps = p.reg(&format!("{APP_NAME}\\Capabilities"));
    set_value(&caps, Some("ApplicationName"), APP_NAME)?;
    set_value(&caps, Some("ApplicationDescription"), "a small windows editor that keeps your drafts between launches")?;
    for ext in EXTENSIONS {
        set_value(&format!("{app}\\SupportedTypes"), Some(ext), "")?;
        set_value(&format!("{classes}\\{ext}\\OpenWithProgids"), Some(PROGID), "")?;
        set_value(&format!("{caps}\\FileAssociations"), Some(ext), PROGID)?;
    }
    set_value(&p.reg("RegisteredApplications"), Some(APP_NAME), &format!("Software\\{APP_NAME}\\Capabilities"))?;
    notify_shell();
    Ok(())
}

fn unregister(p: &Paths) {
    let classes = p.reg("Classes");
    delete_tree(&format!("{classes}\\{PROGID}"));
    delete_tree(&format!("{classes}\\Applications\\{APP_NAME}.exe"));
    for ext in EXTENSIONS {
        delete_value(&format!("{classes}\\{ext}\\OpenWithProgids"), PROGID);
    }
    delete_tree(&p.reg(APP_NAME));
    delete_value(&p.reg("RegisteredApplications"), APP_NAME);
    delete_tree(&p.uninstall_key());
    notify_shell();
}

fn notify_shell() {
    if sandbox().is_some() {
        return;
    }
    // SAFETY: documented notification that file associations changed.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

fn write_uninstall_entry(p: &Paths) -> Result<(), String> {
    let key = p.uninstall_key();
    let exe = p.exe.display().to_string();
    for (name, value) in [
        ("DisplayName", APP_NAME.to_string()),
        ("DisplayVersion", VERSION.to_string()),
        ("Publisher", "lezneve".to_string()),
        ("DisplayIcon", exe.clone()),
        ("InstallLocation", p.dir.display().to_string()),
        ("UninstallString", format!("\"{exe}\" --uninstall")),
    ] {
        set_value(&key, Some(name), &value)?;
    }
    let size_kb = std::fs::metadata(&p.exe).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
    set_dword(&key, "NoModify", 1);
    set_dword(&key, "NoRepair", 1);
    set_dword(&key, "EstimatedSize", size_kb);
    Ok(())
}

fn make_shortcut(target: &Path, at: &Path) -> windows::core::Result<()> {
    if let Some(dir) = at.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let target_w = wide(&target.display().to_string());
    let dir_w = wide(&target.parent().unwrap_or(Path::new("")).display().to_string());
    let at_w = wide(&at.display().to_string());
    // SAFETY: com objects created and released in this scope.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(target_w.as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(dir_w.as_ptr()))?;
        link.SetDescription(w!("a small windows editor that keeps your drafts between launches"))?;
        link.SetIconLocation(PCWSTR(target_w.as_ptr()), 0)?;
        link.cast::<IPersistFile>()?.Save(PCWSTR(at_w.as_ptr()), true)
    }
}

// ---- the running ryad ------------------------------------------------------------

/// asks a running ryad (of this data folder) to close, which writes its
/// session first, and waits for it. its own "could not save" question, if
/// any, is answered by the user meanwhile (up to two minutes).
fn close_running(p: &Paths) -> Result<(), String> {
    let class = crate::ipc::class_for(&p.data);
    // SAFETY: a window lookup, a posted close and a wait on the process.
    unsafe {
        let Ok(hwnd) = FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) else { return Ok(()) };
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process = OpenProcess(PROCESS_SYNCHRONIZE, false, pid).map_err(|e| e.message())?;
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        let exited = WaitForSingleObject(process, 120_000) == WAIT_OBJECT_0;
        let _ = CloseHandle(process);
        if !exited {
            return Err("the running ryad did not close. close it and try again.".into());
        }
    }
    Ok(())
}

// ---- install / uninstall -----------------------------------------------------------

pub fn install(p: &Paths) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    close_running(p)?;
    std::fs::create_dir_all(&p.dir).map_err(|e| format!("could not create {}: {e}", p.dir.display()))?;
    if !same_file(&me, &p.exe) {
        let staging = p.exe.with_extension("exe.new");
        std::fs::copy(&me, &staging).map_err(|e| format!("could not copy ryad: {e}"))?;
        // the old exe can stay locked for a moment after its process exits
        let mut tries = 0;
        loop {
            match std::fs::rename(&staging, &p.exe) {
                Ok(()) => break,
                Err(_) if tries < 50 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&staging);
                    return Err(format!("could not replace {}: {e}", p.exe.display()));
                }
            }
        }
    }
    make_shortcut(&p.exe, &p.shortcut).map_err(|e| format!("could not create the start menu entry: {}", e.message()))?;
    write_uninstall_entry(p).map_err(|e| format!("could not add ryad to windows settings › apps: {e}"))?;
    register(p).map_err(|e| format!("could not add ryad to “open with”: {e}"))?;
    trace!("installed {VERSION}");
    Ok(())
}

/// `bring_back_notepad`: ryad removed windows notepad at install and the user
/// wants it back: its store page opens (reinstalling is one click there).
pub fn uninstall(p: &Paths, bring_back_notepad: bool) -> Result<(), String> {
    close_running(p)?;
    if notepad_was_removed(p) {
        if bring_back_notepad {
            open_uri(NOTEPAD_STORE);
        }
        delete_value(&notepad_mark_key(p), NOTEPAD_MARK);
    }
    let _ = std::fs::remove_file(&p.shortcut);
    unregister(p);
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if same_file(&me, &p.exe) {
        remove_later(&p.dir)?;
    } else {
        let _ = std::fs::remove_dir_all(&p.dir);
    }
    Ok(())
}

/// a running exe cannot delete its own folder: a copy in %TEMP% waits for
/// this process and removes it (kadr does the same).
fn remove_later(dir: &Path) -> Result<(), String> {
    check_removable(dir)?;
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper = std::env::temp_dir().join(format!("{APP_NAME}-uninstall-{}.exe", std::process::id()));
    std::fs::copy(&me, &helper).map_err(|e| format!("could not prepare the removal of {}: {e}", dir.display()))?;
    std::process::Command::new(&helper)
        .arg("--finish-uninstall")
        .arg(dir)
        .arg("--wait")
        .arg(std::process::id().to_string())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not remove {}: {e}", dir.display()))
}

/// only a folder named ryad, never anything else by accident.
fn check_removable(dir: &Path) -> Result<(), String> {
    let s = dir.display().to_string();
    if s.len() < 10 || !s.to_ascii_lowercase().ends_with(&format!("\\{APP_NAME}")) || !dir.is_absolute() {
        return Err(format!("refusing to remove an unexpected folder: {s}"));
    }
    Ok(())
}

/// `ryad --finish-uninstall <dir> --wait <pid>`, run from the temp copy.
pub fn finish_uninstall(dir: &Path, wait_pid: Option<u32>) -> i32 {
    if check_removable(dir).is_err() {
        return 2;
    }
    if let Some(pid) = wait_pid {
        // SAFETY: a wait on the uninstaller's process, closed below.
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let _ = WaitForSingleObject(h, 30_000);
                let _ = CloseHandle(h);
            }
        }
    }
    for _ in 0..50 {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    1
}

// ---- the dialogs -------------------------------------------------------------------

pub fn install_interactive() -> i32 {
    crate::installer::run(crate::installer::Mode::Install)
}

pub fn uninstall_interactive() -> i32 {
    crate::installer::run(crate::installer::Mode::Uninstall)
}

// ---- windows notepad -----------------------------------------------------------------
//
// "replace windows notepad", asked once in the installer like kadr asks about
// snipping tool: after a yes, ryad removes the notepad app (the store package
// Microsoft.WindowsNotepad) for this account only, notes that it did, and
// opens windows settings at ryad's default apps page, where one click makes
// ryad the default for its types (windows does not let programs set defaults
// themselves). uninstalling ryad offers notepad back through its store page.
// the old system notepad.exe is a windows component that needs administrator
// rights; ryad leaves it alone.

const NOTEPAD_FAMILY: &str = "Microsoft.WindowsNotepad_8wekyb3d8bbwe";
const NOTEPAD_STORE: &str = "ms-windows-store://pdp/?ProductId=9MSMLRH6LZF3";
const NOTEPAD_MARK: &str = "notepad_removed";

fn notepad_packages() -> Vec<windows::core::HSTRING> {
    use windows::Management::Deployment::PackageManager;
    let Ok(pm) = PackageManager::new() else { return Vec::new() };
    // an empty sid means the current user
    let Ok(found) = pm.FindPackagesByUserSecurityIdPackageFamilyName(&windows::core::HSTRING::new(), &windows::core::HSTRING::from(NOTEPAD_FAMILY)) else {
        return Vec::new();
    };
    found.into_iter().filter_map(|p| p.Id().ok()?.FullName().ok()).collect()
}

/// the notepad app is installed for this account (or, in a test sandbox,
/// always: the sandbox never touches the real package).
pub fn notepad_present() -> bool {
    sandbox().is_some() || !notepad_packages().is_empty()
}

fn notepad_mark_key(p: &Paths) -> String {
    p.reg(&format!("{APP_NAME}\\Setup"))
}

pub fn notepad_was_removed(p: &Paths) -> bool {
    get_value(&notepad_mark_key(p), NOTEPAD_MARK).is_some()
}

/// removes the notepad app for this account and remembers that ryad did.
pub fn replace_notepad(p: &Paths) -> Result<(), String> {
    use windows::Management::Deployment::PackageManager;
    if sandbox().is_none() {
        let pm = PackageManager::new().map_err(|e| e.message())?;
        for full in notepad_packages() {
            let result = pm.RemovePackageAsync(&full).and_then(|op| op.join()).map_err(|e| e.message())?;
            if let Ok(err) = result.ExtendedErrorCode()
                && err.is_err()
            {
                return Err(result.ErrorText().map(|t| t.to_string()).unwrap_or_else(|_| err.message()));
            }
        }
        trace!("installer: windows notepad removed for this account");
    }
    set_value(&notepad_mark_key(p), Some(NOTEPAD_MARK), NOTEPAD_FAMILY)
}

fn open_uri(uri: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    if sandbox().is_some() {
        return;
    }
    let u = wide(uri);
    // SAFETY: a fixed uri (never built from file names), opened by the shell.
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// windows settings › default apps, at ryad's own page (windows 11 with the
/// 2023-04 update or later; older builds show the general page).
pub fn open_default_apps() {
    open_uri(&format!("ms-settings:defaultapps?registeredAppUser={APP_NAME}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_names() {
        for yes in ["ryad-setup", "ryad-0.1.0-setup", "RYAD-0.1.0-setup (2)", "ryad-0.2.0-rc.1-setup"] {
            assert!(is_setup_name(yes), "{yes}");
        }
        for no in ["ryad", "ryad-portable", "setup", "ryad-setup-old", "ryad-x-setup", "ryad-0.1.0-setup (x)"] {
            assert!(!is_setup_name(no), "{no}");
        }
    }

    #[test]
    fn only_a_ryad_folder_can_be_removed() {
        assert!(check_removable(Path::new("C:\\Users\\x\\AppData\\Local\\Programs\\ryad")).is_ok());
        assert!(check_removable(Path::new("C:\\Users\\x\\AppData\\Local\\Programs")).is_err());
        assert!(check_removable(Path::new("ryad")).is_err());
    }
}
