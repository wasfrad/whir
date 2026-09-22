//! Native file dialogs.

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog,
    SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, PCWSTR};

/// `(label, "*.png;*.jpg")` pairs.
pub type Filters<'a> = &'a [(&'a str, &'a str)];

/// Multi-select "Open" dialog. Empty if cancelled. Requires COM (STA) on this thread.
pub fn open_files(owner: HWND, title: &str, filters: Filters<'_>) -> Vec<PathBuf> {
    unsafe { open_files_impl(owner, title, filters) }.unwrap_or_default()
}

unsafe fn open_files_impl(
    owner: HWND,
    title: &str,
    filters: Filters<'_>,
) -> windows::core::Result<Vec<PathBuf>> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let options = dialog.GetOptions()?;
        dialog.SetOptions(options | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
        dialog.SetTitle(&HSTRING::from(title))?;

        // Keep the wide strings alive for the duration of SetFileTypes.
        let owned: Vec<(HSTRING, HSTRING)> = filters
            .iter()
            .map(|(name, spec)| (HSTRING::from(*name), HSTRING::from(*spec)))
            .collect();
        let specs: Vec<COMDLG_FILTERSPEC> = owned
            .iter()
            .map(|(name, spec)| COMDLG_FILTERSPEC {
                pszName: PCWSTR(name.as_ptr()),
                pszSpec: PCWSTR(spec.as_ptr()),
            })
            .collect();
        if !specs.is_empty() {
            dialog.SetFileTypes(&specs)?;
            dialog.SetFileTypeIndex(1)?;
        }

        // Cancel surfaces as an error HRESULT.
        if dialog.Show(Some(owner)).is_err() {
            return Ok(Vec::new());
        }
        let items = dialog.GetResults()?;
        let count = items.GetCount()?;
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let item = items.GetItemAt(i)?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            if let Ok(s) = name.to_string() {
                paths.push(PathBuf::from(s));
            }
            CoTaskMemFree(Some(name.0 as *const _));
        }
        Ok(paths)
    }
}
