//! system dialogs, always owned by the main window so they return focus to
//! it: task dialogs with ryad's own buttons, and the shell's file dialogs.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_COMMON_BUTTON_FLAGS, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_ERROR_ICON, TD_WARNING_ICON, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_POSITION_RELATIVE_TO_WINDOW, TaskDialogIndirect,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FileOpenDialog, FileSaveDialog, IFileOpenDialog,
    IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
use windows::core::PCWSTR;

use crate::{APP_NAME, wide};

/// what `ask` returns for esc, the close button or "cancel". ryad's own
/// button ids are given to windows with an offset, so none of them can be
/// mistaken for IDCANCEL (2) or IDOK (1).
pub const CANCEL: i32 = 0;
const OFFSET: i32 = 100;

/// a plain message for problems before the main window exists.
pub fn error(owner: Option<HWND>, text: &str) {
    let t = wide(text);
    let title = wide(APP_NAME);
    // SAFETY: valid strings for the duration of the call.
    unsafe {
        MessageBoxW(owner, PCWSTR(t.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONWARNING);
    }
}

#[derive(Clone, Copy)]
pub enum Icon {
    None,
    Warning,
    Error,
}

/// a task dialog: `main` is the short statement, `content` the details,
/// `buttons` ryad's own buttons (id, label). esc and the close button give
/// CANCEL. returns the chosen id.
pub fn ask(owner: HWND, icon: Icon, main: &str, content: &str, buttons: &[(i32, &str)], default: i32, cancel_button: bool) -> i32 {
    task(owner, icon, main, content, buttons, default, cancel_button, None).0
}

#[allow(clippy::too_many_arguments)]
fn task(
    owner: HWND,
    icon: Icon,
    main: &str,
    content: &str,
    buttons: &[(i32, &str)],
    default: i32,
    cancel_button: bool,
    check: Option<(&str, bool)>,
) -> (i32, bool) {
    use windows::Win32::UI::Controls::TDF_VERIFICATION_FLAG_CHECKED;
    let title = wide(APP_NAME);
    let main_w = wide(main);
    let content_w = wide(content);
    let labels: Vec<Vec<u16>> = buttons.iter().map(|(_, l)| wide(l)).collect();
    let btns: Vec<TASKDIALOG_BUTTON> =
        buttons.iter().zip(&labels).map(|((id, _), l)| TASKDIALOG_BUTTON { nButtonID: id + OFFSET, pszButtonText: PCWSTR(l.as_ptr()) }).collect();
    let icon_res = match icon {
        Icon::None => PCWSTR::null(),
        Icon::Warning => TD_WARNING_ICON,
        Icon::Error => TD_ERROR_ICON,
    };
    let check_w = check.map(|(t, _)| wide(t));
    let mut flags = TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW;
    if check.is_some_and(|(_, on)| on) {
        flags |= TDF_VERIFICATION_FLAG_CHECKED;
    }
    let cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: flags,
        pszVerificationText: check_w.as_ref().map(|t| PCWSTR(t.as_ptr())).unwrap_or(PCWSTR::null()),
        dwCommonButtons: if cancel_button { TASKDIALOG_COMMON_BUTTON_FLAGS(8) } else { TASKDIALOG_COMMON_BUTTON_FLAGS(0) }, // TDCBF_CANCEL_BUTTON
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: icon_res },
        pszMainInstruction: PCWSTR(main_w.as_ptr()),
        pszContent: if content.is_empty() { PCWSTR::null() } else { PCWSTR(content_w.as_ptr()) },
        cButtons: btns.len() as u32,
        pButtons: btns.as_ptr(),
        nDefaultButton: if default == CANCEL { 2 } else { default + OFFSET },
        ..Default::default()
    };
    let mut chosen = 0;
    let mut verified = windows::core::BOOL(0);
    // SAFETY: every pointer in cfg points at locals that outlive the call.
    let r = unsafe { TaskDialogIndirect(&cfg, Some(&mut chosen), None, Some(&mut verified)) };
    let choice = if r.is_err() || chosen < OFFSET { CANCEL } else { chosen - OFFSET };
    (choice, verified.as_bool())
}

pub fn problem(owner: HWND, main: &str, content: &str) {
    ask(owner, Icon::Warning, main, content, &[(1, "ok")], 1, false);
}

const TYPES: &[(&str, &str)] = &[("text files", "*.txt;*.log;*.md;*.ini;*.cfg;*.json;*.csv;*.xml;*.yaml;*.yml;*.toml"), ("all files", "*.*")];

fn filters() -> (Vec<Vec<u16>>, Vec<COMDLG_FILTERSPEC>) {
    let strings: Vec<Vec<u16>> = TYPES.iter().flat_map(|(n, s)| [wide(n), wide(s)]).collect();
    let specs = strings.chunks(2).map(|c| COMDLG_FILTERSPEC { pszName: PCWSTR(c[0].as_ptr()), pszSpec: PCWSTR(c[1].as_ptr()) }).collect();
    (strings, specs)
}

fn folder_item(dir: &Path) -> Option<IShellItem> {
    let d = wide(&dir.to_string_lossy());
    // SAFETY: valid path string.
    unsafe { SHCreateItemFromParsingName(PCWSTR(d.as_ptr()), None).ok() }
}

fn item_path(item: &IShellItem) -> Option<PathBuf> {
    // SAFETY: the returned string is freed after copying.
    unsafe {
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// the system "open" dialog; several files at once. empty when cancelled.
pub fn open_files(owner: HWND, start: Option<&Path>) -> Vec<PathBuf> {
    let (_keep, specs) = filters();
    // SAFETY: com objects released on drop; the strings outlive the calls.
    unsafe {
        let Ok(d) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else { return Vec::new() };
        let _ = d.SetOptions(FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
        let _ = d.SetFileTypes(&specs);
        let _ = d.SetFileTypeIndex(2); // all files: ryad opens any text file
        if let Some(item) = start.and_then(folder_item) {
            let _ = d.SetFolder(&item);
        }
        if d.Show(Some(owner)).is_err() {
            return Vec::new();
        }
        let Ok(items) = d.GetResults() else { return Vec::new() };
        let n = items.GetCount().unwrap_or(0);
        (0..n).filter_map(|i| items.GetItemAt(i).ok()).filter_map(|it| item_path(&it)).collect()
    }
}

/// the system "save as" dialog. no default extension is forced: files
/// without one are fine.
pub fn save_file(owner: HWND, name: &str, folder: Option<&Path>) -> Option<PathBuf> {
    // test hook: scripted checks cannot drive the shell dialog by messages
    if let Some(p) = std::env::var_os("RYAD_TEST_SAVE_PATH") {
        return (!p.is_empty()).then(|| PathBuf::from(p));
    }
    let (_keep, specs) = filters();
    let n = wide(name);
    // SAFETY: as above.
    unsafe {
        let d = CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let _ = d.SetOptions(FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM);
        let _ = d.SetFileTypes(&specs);
        let _ = d.SetFileTypeIndex(2);
        let _ = d.SetFileName(PCWSTR(n.as_ptr()));
        if let Some(item) = folder.and_then(folder_item) {
            let _ = d.SetFolder(&item);
        }
        d.Show(Some(owner)).ok()?;
        let item = d.GetResult().ok()?;
        item_path(&item)
    }
}
