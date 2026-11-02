//! Panic-safe временные каталоги для тестов.
//!
//! Удаление — в `Drop`, а не в конце теста: красные прогоны больше не
//! мусорят в `%TEMP%` каталогами `mapinstaller-*-<pid>-<nanos>`.

use std::ops::Deref;
use std::path::{Path, PathBuf};

#[cfg(windows)]
pub fn windows_dacl(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

    struct LocalMemory(*mut std::ffi::c_void);
    impl Drop for LocalMemory {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0); }
        }
    }

    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(path.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(),
            std::ptr::null_mut(), &mut descriptor)
    };
    assert_eq!(status, 0, "read directory ACL");
    let descriptor = LocalMemory(descriptor);
    let mut text = std::ptr::null_mut();
    let mut length = 0;
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(descriptor.0, SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION, &mut text, &mut length)
    };
    assert_ne!(converted, 0, "convert directory ACL");
    let allocation = LocalMemory(text.cast());
    let value = unsafe { std::slice::from_raw_parts(text, length as usize) };
    let end = value.iter().position(|&c| c == 0).unwrap_or(value.len());
    let value = String::from_utf16(&value[..end]).expect("SDDL");
    drop(allocation);
    value
}

pub fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    write_zip_with_options(path, entries, zip::write::SimpleFileOptions::default());
}

pub fn write_stored_zip(path: &Path, entries: &[(&str, &[u8])]) {
    write_zip_with_options(path, entries, zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored));
}

fn write_zip_with_options(path: &Path, entries: &[(&str, &[u8])], options: zip::write::SimpleFileOptions) {
    use std::io::Write;
    let file = std::fs::File::create(path).expect("create zip");
    let mut zip = zip::ZipWriter::new(file);
    for (name, body) in entries {
        zip.start_file(*name, options).expect("entry");
        zip.write_all(body).expect("write");
    }
    zip.finish().expect("finish");
}

pub async fn serve_http_once(body: Vec<u8>, delay: std::time::Duration) -> (String, tokio::task::JoinHandle<()>) {
    serve_http_sequence(vec![body], delay).await
}

pub async fn serve_http_sequence(bodies: Vec<Vec<u8>>, delay: std::time::Duration) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("fixture server");
    let url = format!("http://{}/file", listener.local_addr().expect("fixture address"));
    let server = tokio::spawn(async move {
        for body in bodies {
            let (mut stream, _) = listener.accept().await.expect("fixture connection");
            let _ = stream.read(&mut [0u8; 4096]).await;
            let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(header.as_bytes()).await;
            tokio::time::sleep(delay).await;
            let _ = stream.write_all(&body).await;
        }
    });
    (url, server)
}

pub fn compat_root(parent: &Path) -> crate::services::compat_temp::CompatLease {
    crate::services::compat_temp::CompatTemp::default().root_in(parent).expect("private compat temp")
}

pub async fn wait_for_entry_count(path: &Path, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while std::fs::read_dir(path).expect("fixture directory").count() != expected {
            tokio::task::yield_now().await;
        }
    }).await.expect("temporary file owners must finish cleanup");
}

pub fn link_directory(target: &Path, link: &Path) {
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("create junction");
        assert!(
            status.status.success(),
            "junction: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("create symlink");
}

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
