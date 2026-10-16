//! Panic-safe временные каталоги для тестов.
//!
//! Удаление — в `Drop`, а не в конце теста: красные прогоны больше не
//! мусорят в `%TEMP%` каталогами `mapinstaller-*-<pid>-<nanos>`.

use std::ops::Deref;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Создаёт `%TEMP%/{prefix}-{pid}-{nanos}`.
    pub fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp root");
        Self { path }
    }
}

impl Deref for TempDir {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

// Сравнения с PathBuf для тестовых assert'ов (сравниваем по пути).
impl PartialEq<PathBuf> for TempDir {
    fn eq(&self, other: &PathBuf) -> bool {
        self.path == *other
    }
}

impl PartialEq<TempDir> for PathBuf {
    fn eq(&self, other: &TempDir) -> bool {
        *self == other.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}
