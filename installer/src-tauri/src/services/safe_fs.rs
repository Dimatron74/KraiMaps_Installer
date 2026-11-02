//! Запись относительно открытого каталога: ссылки не обходятся, файл
//! заменяется атомарно, чтобы не менять другие имена его hard link.

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use super::work_budget::WorkBudget;

pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
}

pub struct SafeDir {
    dir: Dir,
    budget: WorkBudget,
}

impl SafeDir {
    pub fn open(root: &Path) -> io::Result<Self> {
        Self::with_budget(root, WorkBudget::default())
    }

    pub fn with_budget(root: &Path, budget: WorkBudget) -> io::Result<Self> {
        budget.check()?;
        let parent = root.parent().ok_or_else(invalid_path)?;
        let name = root.file_name().ok_or_else(invalid_path)?;
        let parent = Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
        Ok(Self {
            dir: parent.open_dir_nofollow(name)?,
            budget,
        })
    }

    fn parent<'a>(&self, path: &'a Path, create: bool) -> io::Result<(Dir, &'a std::ffi::OsStr)> {
        self.budget.check()?;
        let parts: Vec<_> = path.components().collect();
        if parts.is_empty() || parts.iter().any(|part| !matches!(part, Component::Normal(_))) {
            return Err(invalid_path());
        }
        let mut dir = self.dir.try_clone()?;
        for part in &parts[..parts.len() - 1] {
            self.budget.check()?;
            let name = part.as_os_str();
            dir = match dir.open_dir_nofollow(name) {
                Ok(child) => child,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    self.budget.check()?;
                    match self.budget.commit(|| dir.create_dir(name)) {
                        Ok(()) => {},
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
                        Err(error) => return Err(error),
                    }
                    dir.open_dir_nofollow(name)?
                }
                Err(error) => return Err(error),
            };
        }
        Ok((dir, parts.last().ok_or_else(invalid_path)?.as_os_str()))
    }

    pub fn open_file(&self, path: &Path) -> io::Result<File> {
        self.open_regular_file(path, false)
    }

    pub fn open_read_write(&self, path: &Path) -> io::Result<File> {
        self.open_regular_file(path, true)
    }

    fn open_regular_file(&self, path: &Path, write: bool) -> io::Result<File> {
        let (parent, name) = self.parent(path, false)?;
        let mut options = OpenOptions::new();
        options.read(true).write(write).follow(FollowSymlinks::No);
        let file = parent.open_with(name, &options)?;
        self.budget.check()?;
        if !file.metadata()?.is_file() {
            return Err(invalid_path());
        }
        Ok(file)
    }

    pub fn open_subdir(&self, path: &Path) -> io::Result<Self> {
        let (parent, name) = self.parent(path, false)?;
        Ok(Self {
            dir: parent.open_dir_nofollow(name)?,
            budget: self.budget.clone(),
        })
    }

    pub fn read_bounded(&self, path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
        let mut file = self.open_file(path)?;
        let size = file.metadata()?.len();
        if size > max_bytes {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "файл превышает лимит размера"));
        }
        let mut bytes = Vec::with_capacity(size as usize);
        self.budget.copy(&mut (&mut file).take(max_bytes + 1), &mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "файл превышает лимит размера"));
        }
        Ok(bytes)
    }

    pub fn remove_file(&self, path: &Path) -> io::Result<()> {
        let (parent, name) = self.parent(path, false)?;
        require_regular_or_missing(&parent, name)?;
        self.budget.commit(|| parent.remove_file(name))
    }

    pub fn remove_tree(self) -> io::Result<()> {
        self.budget.commit(|| self.dir.remove_open_dir_all())
    }

    pub fn files(&self) -> io::Result<Vec<FileEntry>> {
        let mut files = Vec::new();
        collect_files(&self.dir, Path::new(""), &mut files, &self.budget)?;
        Ok(files)
    }

    pub fn write_atomic(
        &self,
        path: &Path,
        write: impl FnOnce(&mut AtomicFile) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut output = self.begin_write(path)?;
        write(&mut output)?;
        output.finish()
    }

    pub fn begin_write(&self, path: &Path) -> io::Result<AtomicFile> {
        let (parent, name) = self.parent(path, true)?;
        require_regular_or_missing(&parent, name)?;
        let temporary = format!(".compat-write-{}.part", uuid::Uuid::new_v4().simple());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).follow(FollowSymlinks::No);
        let output = self.budget.commit(|| parent.open_with(&temporary, &options))?;
        let output = AtomicFile {
            parent,
            name: name.to_os_string(),
            temporary: Some(temporary),
            output: Some(output),
            budget: self.budget.clone(),
        };
        self.budget.check()?;
        Ok(output)
    }
}

pub struct AtomicFile {
    parent: Dir,
    name: std::ffi::OsString,
    temporary: Option<String>,
    output: Option<File>,
    budget: WorkBudget,
}

impl AtomicFile {
    pub fn finish(mut self) -> io::Result<()> {
        self.flush()?;
        drop(self.output.take());
        let temporary = self.temporary.as_deref().ok_or_else(invalid_path)?;
        self.budget.commit(|| {
            require_regular_or_missing(&self.parent, &self.name)?;
            self.parent.rename(temporary, &self.parent, &self.name)
        })?;
        self.temporary = None;
        Ok(())
    }
}

impl Write for AtomicFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.budget.check()?;
        let written = self.output.as_mut().ok_or_else(invalid_path)?.write(bytes)?;
        self.budget.check()?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.budget.check()?;
        self.output.as_mut().ok_or_else(invalid_path)?.flush()?;
        self.budget.check()
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        drop(self.output.take());
        if let Some(temporary) = self.temporary.take() {
            // Незавершённая запись удаляется даже после отмены её бюджета.
            let _ = self.parent.remove_file(temporary);
        }
    }
}

fn collect_files(dir: &Dir, prefix: &Path, files: &mut Vec<FileEntry>, budget: &WorkBudget) -> io::Result<()> {
    budget.check()?;
    for entry in dir.entries()? {
        budget.check()?;
        let entry = entry?;
        let name = entry.file_name();
        let path = prefix.join(&name);
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(invalid_path());
        }
        if kind.is_dir() {
            collect_files(&dir.open_dir_nofollow(&name)?, &path, files, budget)?;
        } else if kind.is_file() {
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let file = dir.open_with(&name, &options)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(invalid_path());
            }
            files.push(FileEntry {
                path,
                size: metadata.len(),
            });
        } else {
            return Err(invalid_path());
        }
    }
    Ok(())
}

fn require_regular_or_missing(dir: &Dir, name: &std::ffi::OsStr) -> io::Result<()> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(invalid_path()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn invalid_path() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "путь содержит ссылку или не является обычным файлом")
}
