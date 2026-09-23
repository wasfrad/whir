//! Native file dialogs.

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOREADONLYRETURN,
    FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FileOpenDialog, FileSaveDialog, IFileDialog,
    IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, PCWSTR};

/// `(label, "*.png;*.jpg")` pairs.
pub type Filters<'a> = &'a [(&'a str, &'a str)];

/// What a Save dialog starts with.
pub struct SaveOptions<'a> {
    pub title: &'a str,
    pub suggested_name: &'a str,
    pub filters: Filters<'a>,
    /// Without the dot; appended when the user types a name without one.
    pub default_ext: &'a str,
    pub folder: Option<&'a std::path::Path>,
}

/// Native "Save as" dialog. `None` if cancelled. Requires COM (STA) on this thread.
pub fn save_file(owner: HWND, opts: &SaveOptions<'_>) -> Option<PathBuf> {
    unsafe { save_file_impl(owner, opts) }.ok().flatten()
}

/// Keep the wide strings alive for the duration of SetFileTypes.
unsafe fn set_filters(dialog: &IFileDialog, filters: Filters<'_>) -> windows::core::Result<()> {
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
        unsafe {
            dialog.SetFileTypes(&specs)?;
            dialog.SetFileTypeIndex(1)?;
        }
    }
    Ok(())
}

unsafe fn save_file_impl(
    owner: HWND,
    opts: &SaveOptions<'_>,
) -> windows::core::Result<Option<PathBuf>> {
    unsafe {
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
        let options = dialog.GetOptions()?;
        dialog.SetOptions(
            options
                | FOS_OVERWRITEPROMPT
                | FOS_FORCEFILESYSTEM
                | FOS_PATHMUSTEXIST
                | FOS_NOREADONLYRETURN,
        )?;
        dialog.SetTitle(&HSTRING::from(opts.title))?;
        set_filters(&dialog, opts.filters)?;
        if !opts.default_ext.is_empty() {
            dialog.SetDefaultExtension(&HSTRING::from(opts.default_ext))?;
        }
        if !opts.suggested_name.is_empty() {
            dialog.SetFileName(&HSTRING::from(opts.suggested_name))?;
        }
        if let Some(folder) = opts.folder
            && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                &HSTRING::from(folder.as_os_str()),
                None,
            )
        {
            let _ = dialog.SetFolder(&item);
        }
        // Cancel surfaces as an error HRESULT.
        if dialog.Show(Some(owner)).is_err() {
            return Ok(None);
        }
        let item = dialog.GetResult()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as *const _));
        Ok(path)
    }
}

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
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let options = dialog.GetOptions()?;
        dialog
            .SetOptions(options | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
        dialog.SetTitle(&HSTRING::from(title))?;
        set_filters(&dialog, filters)?;

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
