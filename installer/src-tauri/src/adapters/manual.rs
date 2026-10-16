//! Ручная папка (ManualFolderAdapter): только ванильный деплой мира
//! + открытие папки в проводнике. Кнопки «Играть» здесь нет — лаунчера нет.

use crate::domain::InstallError;
use std::path::Path;

/// Открывает папку в проводнике ОС (для ручной цели вместо «Играть»).
pub fn open_folder(path: &Path) -> Result<String, InstallError> {
    #[cfg(windows)]
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path)
        .spawn()
        .map_err(|e| InstallError::LaunchFailed(format!("open {}: {e}", path.display())))?;
    Ok(path.display().to_string())
}
