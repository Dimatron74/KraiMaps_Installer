//! Приватный temp сеанса: планы живут до reset, staging — до конца попытки.
//! Windows DACL задаётся при создании, а не после появления доступного каталога.

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::safe_fs::SafeDir;

pub const SESSION_PREFIX: &str = "compat-session-";
const OWNER_FILE: &str = ".kraimaps-owner";
const OWNER_MAGIC: &str = "KraiMaps compat temp v1\n";

#[derive(Default)]
pub struct CompatTemp {
    directory: Mutex<Option<CompatLease>>,
}

impl CompatTemp {
    pub fn root_in(&self, parent: &Path) -> io::Result<CompatLease> {
        let mut directory = self.directory.lock().map_err(|_| io::Error::other("temp сеанса недоступен"))?;
        if directory.is_none() {
            *directory = Some(CompatLease(Arc::new(OwnedDir::Session(PrivateDir::new(parent)?))));
        }
        directory.clone().ok_or_else(|| io::Error::other("temp сеанса недоступен"))
    }

    pub fn current_root(&self) -> Option<CompatLease> {
        self.directory.lock().ok()?.clone()
    }

    pub fn release(&self) {
        if let Ok(mut directory) = self.directory.lock() {
            let owned = directory.take();
            drop(directory);
            drop(owned);
        }
    }
}

#[derive(Clone)]
pub struct CompatLease(Arc<OwnedDir>);

enum OwnedDir {
    Session(PrivateDir),
    Stage { directory: tempfile::TempDir, _parent: CompatLease },
}

impl CompatLease {
    pub fn staging(&self) -> io::Result<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix(super::compat::STAGE_PREFIX);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let directory = builder.tempdir_in(self.as_ref())?;
        Ok(Self(Arc::new(OwnedDir::Stage { directory, _parent: self.clone() })))
    }

    pub fn file(&self, relative: &Path) -> io::Result<CompatTempFile> {
        if relative.as_os_str().is_empty()
            || relative.components().any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "файл вне приватного temp"));
        }
        Ok(CompatTempFile(Arc::new(OwnedFile {
            path: self.join(relative),
            relative: relative.to_path_buf(),
            directory: self.clone(),
            kept: AtomicBool::new(false),
        })))
    }
}

impl AsRef<Path> for CompatLease {
    fn as_ref(&self) -> &Path {
        match self.0.as_ref() {
            OwnedDir::Session(directory) => &directory.path,
            OwnedDir::Stage { directory, .. } => directory.path(),
        }
    }
}

impl std::ops::Deref for CompatLease {
    type Target = Path;
    fn deref(&self) -> &Path { self.as_ref() }
}

#[derive(Clone)]
pub struct CompatTempFile(Arc<OwnedFile>);

struct OwnedFile {
    path: PathBuf,
    relative: PathBuf,
    directory: CompatLease,
    kept: AtomicBool,
}

impl CompatTempFile {
    pub fn directory(&self) -> &Path { self.0.directory.as_ref() }
    pub fn relative(&self) -> &Path { &self.0.relative }
    pub fn keep(&self) { self.0.kept.store(true, Ordering::Release); }
}

impl AsRef<Path> for CompatTempFile {
    fn as_ref(&self) -> &Path { &self.0.path }
}

impl std::ops::Deref for CompatTempFile {
    type Target = Path;
    fn deref(&self) -> &Path { self.as_ref() }
}

impl Drop for OwnedFile {
    fn drop(&mut self) {
        if !self.kept.load(Ordering::Acquire) {
            if let Ok(root) = SafeDir::open(self.directory.as_ref()) {
                let _ = root.remove_file(&self.relative);
            }
        }
    }
}

struct PrivateDir {
    path: PathBuf,
    lock: Option<std::fs::File>,
}

impl PrivateDir {
    fn new(parent: &Path) -> io::Result<Self> {
        let path = parent.join(format!("{SESSION_PREFIX}{}", uuid::Uuid::new_v4().simple()));
        #[cfg(windows)]
        create_private_windows_dir(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        }
        let mut directory = Self { path, lock: None };
        let mut lock = std::fs::File::create_new(directory.path.join(OWNER_FILE))?;
        lock.try_lock()?;
        let id = session_id(directory.path.file_name().and_then(|name| name.to_str()).unwrap_or_default())
            .ok_or_else(|| io::Error::other("неверное имя temp сеанса"))?;
        write!(lock, "{OWNER_MAGIC}{id}\n")?;
        lock.sync_data()?;
        directory.lock = Some(lock);
        Ok(directory)
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        // Ошибка очистки не отменяет установку; остатки заберёт возрастной sweep.
        let _ = std::fs::remove_dir_all(&self.path);
        drop(self.lock.take());
    }
}

pub fn session_id(name: &str) -> Option<&str> {
    name.strip_prefix(SESSION_PREFIX)
        .filter(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub fn remove_abandoned_session(path: &Path) -> io::Result<()> {
    let id = path.file_name().and_then(|name| name.to_str()).and_then(session_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "не наш temp сеанса"))?;
    let root = SafeDir::open(path)?;
    let mut lock = root.open_read_write(Path::new(OWNER_FILE))?.into_std();
    lock.try_lock()?;
    require_single_link(&lock)?;
    let expected = format!("{OWNER_MAGIC}{id}\n");
    let mut actual = Vec::new();
    (&mut lock).take(128).read_to_end(&mut actual)?;
    if actual != expected.as_bytes() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "нет подтверждения владения temp"));
    }
    // cap-std закрывает directory handle перед удалением на Windows; lock остаётся у нас.
    root.remove_tree()
}

fn require_single_link(file: &std::fs::File) -> io::Result<()> {
    #[cfg(windows)]
    let links = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };

        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe { info.assume_init() }.nNumberOfLinks as u64
    };
    #[cfg(unix)]
    let links = {
        use std::os::unix::fs::MetadataExt;
        file.metadata()?.nlink()
    };
    if links != 1 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "маркер владения temp имеет дополнительные ссылки"));
    }
    Ok(())
}

#[cfg(windows)]
fn create_private_windows_dir(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorControl, DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
        SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

    struct LocalMemory(*mut std::ffi::c_void);
    impl Drop for LocalMemory {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0); }
        }
    }

    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;OW)".encode_utf16().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1,
            &mut descriptor, std::ptr::null_mut())
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = LocalMemory(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let created = unsafe { CreateDirectoryW(wide_path.as_ptr(), &attributes) };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    let validated = (|| {
        let mut actual = std::ptr::null_mut();
        let status = unsafe {
            GetNamedSecurityInfoW(wide_path.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(),
                std::ptr::null_mut(), &mut actual)
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let actual = LocalMemory(actual);
        let mut control = 0;
        let mut revision = 0;
        let read = unsafe { GetSecurityDescriptorControl(actual.0, &mut control, &mut revision) };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        if control & SE_DACL_PROTECTED == 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "файловая система не поддерживает приватный temp"));
        }
        Ok(())
    })();
    if validated.is_err() {
        let _ = std::fs::remove_dir(path);
    }
    validated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TempDir;

    #[test]
    fn session_keeps_plans_between_calls_and_removes_only_its_directory_on_release() {
        let parent = TempDir::new("compat-private-parent");
        let session = CompatTemp::default();
        let root = session.root_in(&parent).expect("private root");
        assert_ne!(root.as_ref(), parent.as_ref());
        assert_eq!(root.parent(), Some(parent.as_ref()));
        std::fs::write(root.join("compat-plan-test.json"), b"plan").expect("plan");

        assert_eq!(session.root_in(&parent).expect("same session").as_ref(), root.as_ref());
        assert_eq!(std::fs::read(root.join("compat-plan-test.json")).expect("plan"), b"plan");
        let path = root.to_path_buf();
        session.release();
        drop(root);

        assert!(!path.exists());
        assert!(parent.exists());
    }

    #[test]
    fn private_directory_excludes_other_users_at_creation() {
        let parent = TempDir::new("compat-private-permissions");
        let session = CompatTemp::default();
        let root = session.root_in(&parent).expect("private root");
        #[cfg(windows)]
        {
            let dacl = crate::test_utils::windows_dacl(&root);
            assert!(dacl.starts_with("D:P"), "unprotected DACL: {dacl}");
            assert!(dacl.contains(";;;OW)"), "owner access: {dacl}");
            assert!(dacl.contains(";;;SY)"), "system access: {dacl}");
            assert_eq!(dacl.matches("(A;").count(), 2, "unexpected users: {dacl}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&root).expect("root").permissions().mode() & 0o777, 0o700);
        }
    }

    #[test]
    fn dropping_the_session_removes_abandoned_files() {
        let parent = TempDir::new("compat-private-drop");
        let root;
        {
            let session = CompatTemp::default();
            root = session.root_in(&parent).expect("private root").to_path_buf();
            std::fs::write(root.join("compat-pack-test.mrpack"), b"pack").expect("pack");
        }
        assert!(!root.exists());
        assert!(parent.exists());
    }

    #[test]
    fn releasing_a_session_waits_for_active_directory_owners() {
        let parent = TempDir::new("compat-active-lease");
        let session = CompatTemp::default();
        let owner = session.root_in(&parent).expect("private root");
        let path = owner.to_path_buf();
        std::fs::write(owner.join("compat-plan-active.json"), b"plan").expect("plan");

        session.release();

        assert!(path.exists(), "release must not remove a directory used by a worker");
        assert_eq!(std::fs::read(owner.join("compat-plan-active.json")).expect("active plan"), b"plan");
        drop(owner);
        assert!(!path.exists(), "the last owner must remove its directory");
    }

    #[tokio::test]
    async fn cancelled_blocking_writer_keeps_its_session_until_the_worker_stops() {
        use super::super::work_budget::WorkBudget;
        let parent = TempDir::new("compat-blocking-owner");
        let session = CompatTemp::default();
        let root = session.root_in(&parent).expect("session");
        let path = root.to_path_buf();
        let file = root.file(Path::new("compat-pack-active.mrpack")).expect("owned file");
        let budget = WorkBudget::default();
        let worker_budget = budget.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, stop) = std::sync::mpsc::channel();
        let mut attempt = Box::pin(budget.blocking(move || {
            let directory = SafeDir::with_budget(file.directory(), worker_budget)?;
            let mut output = directory.begin_write(file.relative())?;
            output.write_all(b"uncommitted bytes")?;
            let _ = started.send(());
            stop.recv_timeout(std::time::Duration::from_secs(5)).expect("worker release");
            output.finish()
        }));
        tokio::select! {
            result = &mut attempt => panic!("worker must wait for its release: {result:?}"),
            result = ready => result.expect("worker started"),
        }

        drop(attempt);
        session.release();
        drop(root);

        assert!(path.exists(), "the detached writer is still using this directory");
        assert!(remove_abandoned_session(&path).is_err(), "the worker must retain the ownership lock");
        release.send(()).expect("stop writer");
        crate::test_utils::wait_for_entry_count(&parent, 0).await;
        assert!(!path.exists(), "the worker's last lease must clean the whole session");
    }
}
