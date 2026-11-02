//! Сетевой слой слоя совместимости: единый клиент, allowlist хостов,
//! watchdog нулевой скорости, ретраи и проверка хешей.
//!
//! Почему отдельный модуль, а не `site::http_client`: у сайта таймаут 30 с
//! на весь запрос и нет контроля скорости. Скачивание сборки — это десятки
//! файлов по 10–100 МиБ; зависший CDN обязан обрываться с понятной ошибкой,
//! а не держать установку карты минутами (дизайн §7.10).

use crate::domain::InstallError;
use crate::services::compat_temp::CompatTempFile;
use crate::services::safe_fs::{AtomicFile, SafeDir};
use crate::services::work_budget::WorkBudget;
use futures_util::StreamExt;
use sha1::Digest as _;
use sha2::Digest;
use std::io::Write;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Хосты, которым разрешено отдавать файлы `.mrpack`-индекса.
/// Redirect-хосты GitHub добавлены осознанно: `github.com/.../releases/download`
/// отдаёт 302 на `objects.githubusercontent.com`, без них валидные файлы
/// отклонялись бы.
pub const PACK_HOSTS: &[&str] = &[
    "cdn.modrinth.com",
    "github.com",
    "raw.githubusercontent.com",
    "gitlab.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Наши собственные источники (API Modrinth, страница OptiFine, Maven Forge).
const TRUSTED_HOSTS: &[&str] = &[
    "api.modrinth.com",
    "cdn.modrinth.com",
    "github.com",
    "raw.githubusercontent.com",
    "gitlab.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "optifine.net",
    "maven.minecraftforge.net",
    "files.minecraftforge.net",
];

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// Политика скачивания одного файла. Параметризована ради тестов:
/// живой watchdog в 30 с сделал бы юнит-тест недопустимо долгим.
#[derive(Debug, Clone, Copy)]
pub struct DownloadPolicy {
    /// Окно измерения скорости.
    pub low_speed_window: Duration,
    /// Сколько байт должно прийти за окно, иначе — `E_COMPAT_STALLED`.
    pub low_speed_bytes: u64,
    /// Жёсткий лимит времени на один файл.
    pub file_timeout: Duration,
    /// Всего попыток, включая первую (3 = два ретрая).
    pub attempts: u32,
    /// Пауза перед первым ретраем (дальше удваивается).
    pub retry_backoff: Duration,
}

impl Default for DownloadPolicy {
    fn default() -> Self {
        Self {
            low_speed_window: Duration::from_secs(30),
            // ~1 КиБ/с: сервер, который отдаёт меньше, для нас «стоит».
            low_speed_bytes: 30 * 1024,
            file_timeout: Duration::from_secs(5 * 60),
            attempts: 3,
            retry_backoff: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub bytes: u64,
    pub content_type: String,
    pub content_disposition: Option<String>,
    /// Финальный URL после редиректов: вызывающий проверяет, что файл
    /// действительно пришёл с ожидаемого хоста (OptiFine — только optifine.net).
    pub final_url: String,
}

/// Ожидания по файлу. `size` — из индекса/API; `None` — размер неизвестен
/// (OptiFine до первого байта).
#[derive(Debug, Clone, Copy, Default)]
pub struct Expected<'a> {
    pub size: Option<u64>,
    pub sha512: Option<&'a str>,
    pub sha1: Option<&'a str>,
    pub max_bytes: u64,
}

/// Ответ небольшого GET (страница/JSON).
#[derive(Debug, Clone)]
pub struct Fetched {
    pub bytes: Vec<u8>,
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// URL разрешён для наших запросов: только HTTPS и только известные хосты.
/// В тестах дополнительно разрешён HTTP на loopback — иначе watchdog
/// не проверить без реальной сети.
pub fn url_allowed(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    if !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    let host = u.host_str().unwrap_or_default();
    let scheme_ok = u.scheme() == "https"
        || (cfg!(test) && u.scheme() == "http" && is_loopback(host));
    scheme_ok && (TRUSTED_HOSTS.contains(&host) || (cfg!(test) && is_loopback(host)))
}

/// URL файла из `.mrpack`-индекса: строже — только HTTPS и только
/// CDN-хосты, отдающие файлы модов. API-хосты сюда не входят.
pub fn pack_url_allowed(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    if !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    let host = u.host_str().unwrap_or_default();
    #[cfg(test)]
    if u.scheme() == "http" && is_loopback(host) {
        return true;
    }
    u.scheme() == "https" && PACK_HOSTS.contains(&host)
}

fn redirect_allowed(source: &url::Url, target: &url::Url) -> bool {
    if !url_allowed(source.as_str()) || !url_allowed(target.as_str()) {
        return false;
    }
    if pack_url_allowed(source.as_str()) {
        pack_url_allowed(target.as_str())
    } else {
        source.origin() == target.origin()
    }
}

fn client() -> Result<reqwest::Client, InstallError> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| build_client().map_err(|error| error.details()))
        .as_ref()
        .map(Clone::clone)
        .map_err(|details| InstallError::CompatDownload(details.clone()))
}

fn build_client() -> Result<reqwest::Client, InstallError> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .user_agent(crate::services::site::installer_user_agent())
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let source = attempt.previous().first().unwrap_or_else(|| attempt.url());
            if attempt.previous().len() > 10 {
                attempt.stop()
            } else if redirect_allowed(source, attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|e| InstallError::CompatDownload(format!("http client: {e}")))
}

fn retryable(err: &InstallError) -> bool {
    matches!(
        err,
        InstallError::CompatDownload(_) | InstallError::CompatStalled(_) | InstallError::Io(_)
    )
}

fn status_error(status: reqwest::StatusCode, label: &str) -> InstallError {
    InstallError::CompatDownload(format!("{label}: http {status}"))
}

fn request_source(raw: &str) -> String {
    // В query может жить download-токен; для диагностики достаточно хоста и пути.
    url::Url::parse(raw)
        .ok()
        .and_then(|url| url.host_str().map(|host| format!("{host}{}", url.path())))
        .unwrap_or_else(|| "источник".to_string())
}

/// Небольшой GET с ретраями. `max_bytes` защищает от гигантской страницы.
pub async fn get(url: &str, max_bytes: u64, attempts: u32) -> Result<Fetched, InstallError> {
    let source = request_source(url);
    if !url_allowed(url) {
        return Err(InstallError::CompatInvalid(format!(
            "запрос на недоверенный адрес отклонён: {source}"
        )));
    }
    let client = client()?;
    let mut last: Option<InstallError> = None;
    for attempt in 0..attempts.max(1) {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
        }
        let resp = match client
            .get(url)
            .timeout(DownloadPolicy::default().file_timeout)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last = Some(InstallError::CompatDownload(format!("{source}: {}", e.without_url())));
                continue;
            }
        };
        if !resp.status().is_success() {
            let err = status_error(resp.status(), &source);
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                || resp.status().is_server_error()
            {
                last = Some(err);
                continue;
            }
            return Err(err);
        }
        if let Some(host) = resp.url().host_str() {
            if !url_allowed(resp.url().as_str()) {
                return Err(InstallError::CompatInvalid(format!(
                    "редирект на недоверенный хост: {host}"
                )));
            }
        }
        if let Some(len) = resp.content_length() {
            if len > max_bytes {
                return Err(InstallError::CompatInvalid(format!(
                    "{source}: ответ больше лимита ({len} > {max_bytes})"
                )));
            }
        }
        let bytes = match read_limited_response(resp, max_bytes, &source).await {
            Ok(bytes) => bytes,
            Err(e) => {
                if !retryable(&e) {
                    return Err(e);
                }
                last = Some(e);
                continue;
            }
        };
        return Ok(Fetched { bytes });
    }
    Err(last.unwrap_or_else(|| InstallError::CompatDownload(format!("{source}: нет попыток"))))
}

async fn read_limited_response(
    response: reqwest::Response,
    max_bytes: u64,
    source: &str,
) -> Result<Vec<u8>, InstallError> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            InstallError::CompatDownload(format!("{source}: {}", e.without_url()))
        })?;
        let received = (bytes.len() as u64).saturating_add(chunk.len() as u64);
        if received > max_bytes {
            return Err(InstallError::CompatInvalid(format!(
                "{source}: ответ больше лимита ({received} > {max_bytes})"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub async fn get_text(url: &str, max_bytes: u64, attempts: u32) -> Result<String, InstallError> {
    let fetched = get(url, max_bytes, attempts).await?;
    Ok(String::from_utf8_lossy(&fetched.bytes).into_owned())
}

fn header(resp: &reqwest::Response, name: reqwest::header::HeaderName) -> String {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

#[cfg(test)]
pub async fn download(
    url: &str,
    dest: &Path,
    expected: Expected<'_>,
    label: &str,
    progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    policy: DownloadPolicy,
) -> Result<DownloadOutcome, InstallError> {
    let root = dest.parent().ok_or_else(|| InstallError::CompatApply("у файла нет каталога".into()))?;
    let relative = dest.file_name().ok_or_else(|| InstallError::CompatApply("у файла нет имени".into()))?;
    download_to(url, DownloadDestination {
        root: root.to_path_buf(),
        relative: PathBuf::from(relative),
        _owner: None,
    }, expected, label, progress, policy, &WorkBudget::default()).await
}

pub async fn download_owned(
    url: &str,
    dest: &CompatTempFile,
    expected: Expected<'_>,
    label: &str,
    progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    policy: DownloadPolicy,
    parent: &WorkBudget,
) -> Result<DownloadOutcome, InstallError> {
    download_to(url, DownloadDestination {
        root: dest.directory().to_path_buf(),
        relative: dest.relative().to_path_buf(),
        _owner: Some(dest.clone()),
    }, expected, label, progress, policy, parent).await
}

#[derive(Clone)]
struct DownloadDestination {
    root: PathBuf,
    relative: PathBuf,
    _owner: Option<CompatTempFile>,
}

async fn download_to(
    url: &str,
    dest: DownloadDestination,
    expected: Expected<'_>,
    label: &str,
    progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    policy: DownloadPolicy,
    parent: &WorkBudget,
) -> Result<DownloadOutcome, InstallError> {
    if !url_allowed(url) {
        return Err(InstallError::CompatInvalid(format!(
            "скачивание с недоверенного адреса отклонено: {label}"
        )));
    }
    let budget = parent.limited_to(policy.file_timeout);
    let _cancel = budget.cancel_on_drop();
    budget.network(download_with_retries(url, &dest, expected, label, progress, policy, &budget))
        .await.map_err(|error| download_io_error(label, error))?
}

async fn download_with_retries(
    url: &str,
    dest: &DownloadDestination,
    expected: Expected<'_>,
    label: &str,
    progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    policy: DownloadPolicy,
    budget: &WorkBudget,
) -> Result<DownloadOutcome, InstallError> {
    let mut last: Option<InstallError> = None;
    let high_water = AtomicU64::new(0);
    let monotonic = |done: u64, total: u64| {
        let done = expected.size.map_or(done, |size| done.min(size));
        let previous = high_water.fetch_max(done, Ordering::Relaxed);
        if done > previous {
            if let Some(callback) = progress {
                callback(done, total);
            }
        }
    };
    for attempt in 0..policy.attempts.max(1) {
        budget.check().map_err(|error| download_io_error(label, error))?;
        if attempt > 0 {
            let backoff = policy.retry_backoff.saturating_mul(1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX));
            tokio::time::sleep(backoff).await;
        }
        match download_once(url, dest, expected, label, Some(&monotonic), policy, budget).await {
            Ok(outcome) => return Ok(outcome),
            Err(e) => {
                if !retryable(&e) {
                    return Err(e);
                }
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        InstallError::CompatDownload(format!("{label}: нет попыток"))
    }))
}

async fn download_once(
    url: &str,
    dest: &DownloadDestination,
    expected: Expected<'_>,
    label: &str,
    progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    policy: DownloadPolicy,
    budget: &WorkBudget,
) -> Result<DownloadOutcome, InstallError> {
    budget.check().map_err(|error| download_io_error(label, error))?;
    let client = client()?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| InstallError::CompatDownload(format!("{label}: {}", e.without_url())))?;
    if !resp.status().is_success() {
        return Err(status_error(resp.status(), label));
    }
    if !url_allowed(resp.url().as_str()) {
        return Err(InstallError::CompatInvalid(format!(
            "{label}: редирект на недоверенный хост"
        )));
    }
    if let Some(len) = resp.content_length() {
        if len > expected.max_bytes {
            return Err(InstallError::CompatInvalid(format!(
                "{label}: файл больше лимита ({len} > {})",
                expected.max_bytes
            )));
        }
        if let Some(want) = expected.size {
            if len != want {
                return Err(InstallError::CompatDownload(format!(
                    "{label}: размер не совпал ({len} != {want})"
                )));
            }
        }
    }
    // Заголовки нужны до `bytes_stream()`: после него ответ уже израсходован.
    // OptiFine по ним отличает jar от HTML-заглушки с кодом 200.
    let content_type = header(&resp, reqwest::header::CONTENT_TYPE);
    let final_url = resp.url().to_string();
    let content_disposition = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let (owned_dest, worker_budget) = (dest.clone(), budget.clone());
    let mut writer = budget.blocking(move || DownloadWriter::new(owned_dest, worker_budget))
        .await.map_err(|error| download_io_error(label, error))?
        .map_err(|error| download_io_error(label, error))?;
    let mut done = 0u64;
    let mut window_end = tokio::time::Instant::now() + policy.low_speed_window;
    let mut window_bytes = 0u64;
    let total = expected.size.unwrap_or(0);
    let mut stream = resp.bytes_stream();

    loop {
        budget.check().map_err(|error| download_io_error(label, error))?;
        let next = tokio::select! {
            _ = tokio::time::sleep_until(window_end) => {
                if window_bytes < policy.low_speed_bytes {
                    return Err(InstallError::CompatStalled(format!(
                        "{label}: скорость около нуля ({window_bytes} байт за {:?})",
                        policy.low_speed_window
                    )));
                }
                window_end = tokio::time::Instant::now() + policy.low_speed_window;
                window_bytes = 0;
                continue;
            }
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = next else {
            break;
        };
        let bytes = chunk.map_err(|e| {
            // reqwest сам обрывает чтение по read_timeout — это ровно
            // «сервер не отдаёт данные», а не прочий сетевой сбой.
            InstallError::CompatStalled(format!("{label}: {}", e.without_url()))
        })?;
        done = done.saturating_add(bytes.len() as u64);
        window_bytes = window_bytes.saturating_add(bytes.len() as u64);
        if done > expected.max_bytes {
            return Err(InstallError::CompatInvalid(format!(
                "{label}: файл больше лимита ({done} > {})",
                expected.max_bytes
            )));
        }
        writer = budget.blocking(move || {
            writer.write(&bytes)?;
            Ok::<_, std::io::Error>(writer)
        }).await.map_err(|error| download_io_error(label, error))?
            .map_err(|error| download_io_error(label, error))?;
        if let Some(cb) = progress {
            cb(done, total);
        }
    }
    if let Some(want) = expected.size {
        if done != want {
            return Err(InstallError::CompatDownload(format!(
                "{label}: получено {done} из {want} байт"
            )));
        }
    }
    let (sha512, sha1, owned_label) = (expected.sha512.map(str::to_string), expected.sha1.map(str::to_string), label.to_string());
    budget.blocking(move || writer.finish(sha512.as_deref(), sha1.as_deref(), &owned_label))
        .await.map_err(|error| download_io_error(label, error))??;
    Ok(DownloadOutcome {
        bytes: done,
        content_type,
        content_disposition,
        final_url,
    })
}

struct DownloadWriter {
    output: AtomicFile,
    sha512: sha2::Sha512,
    sha1: sha1::Sha1,
    budget: WorkBudget,
    _destination: DownloadDestination,
}

impl DownloadWriter {
    fn new(destination: DownloadDestination, budget: WorkBudget) -> std::io::Result<Self> {
        let output = SafeDir::with_budget(&destination.root, budget.clone())?
            .begin_write(&destination.relative)?;
        Ok(Self { output, sha512: sha2::Sha512::new(), sha1: sha1::Sha1::new(),
            budget, _destination: destination })
    }

    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        for chunk in bytes.chunks(64 * 1024) {
            self.budget.check()?;
            self.sha512.update(chunk);
            self.sha1.update(chunk);
            self.output.write_all(chunk)?;
        }
        self.budget.check()
    }

    fn finish(self, sha512: Option<&str>, sha1: Option<&str>, label: &str) -> Result<(), InstallError> {
        self.budget.check().map_err(|error| download_io_error(label, error))?;
        if sha512.is_some_and(|want| !want.eq_ignore_ascii_case(&hex(self.sha512.finalize()))) {
            return Err(InstallError::CompatInvalid(format!("{label}: SHA-512 не сошёлся")));
        }
        if sha1.is_some_and(|want| !want.eq_ignore_ascii_case(&hex(self.sha1.finalize()))) {
            return Err(InstallError::CompatInvalid(format!("{label}: SHA-1 не сошёлся")));
        }
        self.output.finish().map_err(|error| download_io_error(label, error))
    }
}

fn download_io_error(label: &str, error: std::io::Error) -> InstallError {
    if error.kind() == std::io::ErrorKind::TimedOut {
        InstallError::CompatStalled(format!("{label}: превышено время скачивания с учётом повторов"))
    } else {
        InstallError::CompatApply(format!("{label}: {error}"))
    }
}

pub(crate) fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
pub fn sha512_file(path: &Path) -> Result<String, InstallError> {
    hash_file::<sha2::Sha512>(path)
}

#[cfg(test)]
fn hash_file<D: Digest + Default>(path: &Path) -> Result<String, InstallError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = D::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn serve_once(response: impl FnOnce(TcpStream) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout");
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            response(stream);
        });
        format!("http://{addr}/file")
    }

    fn spawn_server(
        body: &'static [u8],
        chunk: usize,
        pause: Duration,
        status: &'static str,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if stream.write_all(header.as_bytes()).is_err() {
                    continue;
                }
                for part in body.chunks(chunk.max(1)) {
                    if stream.write_all(part).is_err() {
                        break;
                    }
                    if !pause.is_zero() {
                        std::thread::sleep(pause);
                    }
                }
            }
        });
        format!("http://{addr}/file")
    }

    fn temp_file(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "kraimaps-http-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn test_policy() -> DownloadPolicy {
        DownloadPolicy {
            low_speed_window: Duration::from_millis(150),
            low_speed_bytes: 1024,
            file_timeout: Duration::from_secs(5),
            attempts: 2,
            retry_backoff: Duration::from_millis(10),
        }
    }

    #[test]
    fn url_gate_allows_only_https_and_known_hosts() {
        assert!(url_allowed("https://cdn.modrinth.com/data/x/file.jar"));
        assert!(url_allowed("https://optifine.net/downloadx?f=x.jar&x=abc"));
        assert!(!url_allowed("http://cdn.modrinth.com/data/x/file.jar"));
        assert!(!url_allowed("https://evil.example/file.jar"));
        assert!(!url_allowed("https://user:pass@cdn.modrinth.com/file.jar"));
        assert!(!url_allowed("not a url"));
        // Индекс не может тянуть файлы с API-хостов и optifine.net.
        assert!(pack_url_allowed("https://cdn.modrinth.com/data/x/file.jar"));
        assert!(pack_url_allowed("https://objects.githubusercontent.com/x"));
        assert!(!pack_url_allowed("https://api.modrinth.com/v2/project/x"));
        assert!(!pack_url_allowed("https://optifine.net/downloadx?f=x"));
    }

    #[test]
    fn optifine_redirects_cannot_leave_the_original_origin() {
        let source = url::Url::parse("https://optifine.net/adloadx?f=OptiFine.jar").unwrap();
        for (target, allowed) in [
            ("https://optifine.net/downloadx?f=OptiFine.jar&x=token", true),
            ("https://cdn.modrinth.com/OptiFine.jar", false),
            ("https://api.modrinth.com/v2/project/x", false),
            ("https://maven.minecraftforge.net/OptiFine.jar", false),
            ("https://optifine.net:8443/downloadx", false),
            ("http://optifine.net/downloadx", false),
        ] {
            assert_eq!(
                redirect_allowed(&source, &url::Url::parse(target).unwrap()),
                allowed,
                "{target}"
            );
        }
    }

    #[test]
    fn pack_redirects_cannot_switch_to_api_or_loader_sources() {
        let source = url::Url::parse("https://github.com/owner/mod/releases/download/mod.jar").unwrap();
        for (target, allowed) in [
            ("https://release-assets.githubusercontent.com/mod.jar", true),
            ("https://objects.githubusercontent.com/mod.jar", true),
            ("https://cdn.modrinth.com/data/x/mod.jar", true),
            ("https://api.modrinth.com/v2/project/x", false),
            ("https://maven.minecraftforge.net/mod.jar", false),
            ("https://optifine.net/downloadx", false),
            ("https://evil.example/mod.jar", false),
        ] {
            assert_eq!(
                redirect_allowed(&source, &url::Url::parse(target).unwrap()),
                allowed,
                "{target}"
            );
        }
    }

    #[tokio::test]
    async fn download_verifies_sha512_and_size() {
        let body: &'static [u8] = b"kraimaps-compat-payload";
        let url = spawn_server(body, 8, Duration::ZERO, "200 OK");
        let dest = temp_file("ok.bin");
        let sha512 = {
            let mut h = sha2::Sha512::new();
            h.update(body);
            hex(h.finalize())
        };
        let outcome = download(
            &url,
            &dest,
            Expected {
                size: Some(body.len() as u64),
                sha512: Some(&sha512),
                sha1: None,
                max_bytes: 1024,
            },
            "test",
            None,
            test_policy(),
        )
        .await
        .expect("download");
        assert_eq!(outcome.bytes, body.len() as u64);
        assert_eq!(std::fs::read(&dest).expect("read"), body);
        std::fs::remove_file(&dest).ok();
    }

    #[tokio::test]
    async fn hash_mismatch_is_invalid_and_not_retried() {
        let url = spawn_server(b"payload", 64, Duration::ZERO, "200 OK");
        let dest = temp_file("bad.bin");
        let err = download(
            &url,
            &dest,
            Expected {
                size: Some(7),
                sha512: Some("00"),
                sha1: None,
                max_bytes: 1024,
            },
            "test",
            None,
            test_policy(),
        )
        .await
        .expect_err("must fail");
        assert_eq!(err.code(), "E_COMPAT_INVALID");
        assert!(!dest.exists(), "битый файл удалён");
    }

    #[tokio::test]
    async fn zero_speed_is_stalled() {
        // Заголовок обещает много, тело приходит по байту раз в 200 мс:
        // окно 150 мс и порог 1 КиБ срабатывают почти сразу.
        let url = spawn_server(b"0123456789", 1, Duration::from_millis(200), "200 OK");
        let dest = temp_file("stall.bin");
        let err = download(
            &url,
            &dest,
            Expected {
                size: None,
                sha512: None,
                sha1: None,
                max_bytes: 1024 * 1024,
            },
            "test",
            None,
            test_policy(),
        )
        .await
        .expect_err("must stall");
        assert_eq!(err.code(), "E_COMPAT_STALLED");
        assert!(!dest.exists(), "недокачанный файл удалён");
    }

    #[tokio::test]
    async fn file_deadline_includes_waiting_for_headers() {
        let url = serve_once(|mut stream| {
            std::thread::sleep(Duration::from_millis(300));
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            );
        });
        let dest = temp_file("headers-timeout.bin");
        let policy = DownloadPolicy {
            file_timeout: Duration::from_millis(50),
            attempts: 1,
            ..test_policy()
        };

        let error = download(
            &url,
            &dest,
            Expected { max_bytes: 1024, ..Expected::default() },
            "OptiFine.jar",
            None,
            policy,
        )
        .await
        .expect_err("header wait must respect file deadline");

        assert_eq!(error.code(), "E_COMPAT_STALLED");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn file_deadline_includes_retry_backoff() {
        let url = serve_once(|mut stream| {
            let _ = stream.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });
        let dest = temp_file("backoff-timeout.bin");
        let policy = DownloadPolicy {
            file_timeout: Duration::from_millis(50),
            retry_backoff: Duration::from_millis(500),
            attempts: 2,
            ..test_policy()
        };

        let error = tokio::time::timeout(
            Duration::from_millis(300),
            download(
                &url,
                &dest,
                Expected { max_bytes: 1024, ..Expected::default() },
                "OptiFine.jar",
                None,
                policy,
            ),
        )
        .await
        .expect("retry delay must respect the file deadline")
        .expect_err("file deadline");

        assert_eq!(error.code(), "E_COMPAT_STALLED");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn watchdog_detects_silence_without_waiting_for_another_chunk() {
        let url = serve_once(|mut stream| {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n",
            );
            std::thread::sleep(Duration::from_millis(300));
            let _ = stream.write_all(&[b'x'; 4096]);
        });
        let dest = temp_file("silent.bin");
        let policy = DownloadPolicy {
            low_speed_window: Duration::from_millis(50),
            attempts: 1,
            ..test_policy()
        };

        let error = download(
            &url,
            &dest,
            Expected { max_bytes: 4096, ..Expected::default() },
            "OptiFine.jar",
            None,
            policy,
        )
        .await
        .expect_err("watchdog must fire while body is silent");

        assert_eq!(error.code(), "E_COMPAT_STALLED");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn get_rejects_oversized_chunked_body_before_stream_finishes() {
        let url = serve_once(|mut stream| {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n80\r\n",
            );
            let _ = stream.write_all(&[b'x'; 128]);
            let _ = stream.write_all(b"\r\n");
            std::thread::sleep(Duration::from_millis(1500));
            let _ = stream.write_all(b"0\r\n\r\n");
        });

        let error = tokio::time::timeout(Duration::from_millis(500), get(&url, 64, 1))
            .await
            .expect("size limit must be checked while streaming")
            .expect_err("oversized body");
        assert_eq!(error.code(), "E_COMPAT_INVALID");
    }

    #[tokio::test]
    async fn download_errors_do_not_disclose_query_tokens() {
        let url = format!("{}?f=OptiFine.jar&x=test-secret-token", serve_once(|_| {}));
        let dest = temp_file("redacted-download.bin");
        let error = download(
            &url,
            &dest,
            Expected { max_bytes: 1024, ..Expected::default() },
            "OptiFine.jar",
            None,
            DownloadPolicy { attempts: 1, ..test_policy() },
        )
        .await
        .expect_err("connection closed");

        assert!(!error.details().contains("test-secret-token"));
        assert!(!error.details().contains("?f="));
    }

    #[tokio::test]
    async fn get_errors_do_not_disclose_query_tokens() {
        let url = format!("{}?f=OptiFine.jar&x=test-secret-token", serve_once(|_| {}));
        let error = get(&url, 1024, 1).await.expect_err("connection closed");

        assert!(!error.details().contains("test-secret-token"));
        assert!(!error.details().contains("?f="));
    }

    #[tokio::test]
    async fn repeated_requests_reuse_the_same_connection_pool() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}/file", listener.local_addr().expect("addr"));
        let (sent, mut connections) = tokio::sync::mpsc::channel(2);
        let server = tokio::spawn(async move {
            let mut id = 0;
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let sent = sent.clone();
                let connection_id = id;
                id += 1;
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    loop {
                        let Ok(size) = stream.read(&mut buffer).await else { break };
                        if size == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..size]);
                        if !request.windows(4).any(|window| window == b"\r\n\r\n") {
                            continue;
                        }
                        request.clear();
                        if sent.send(connection_id).await.is_err() {
                            break;
                        }
                        if stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
        });

        assert_eq!(get(&url, 64, 1).await.expect("first response").bytes, b"ok");
        assert_eq!(get(&url, 64, 1).await.expect("second response").bytes, b"ok");
        server.abort();

        assert_eq!(connections.recv().await, Some(0));
        assert_eq!(connections.recv().await, Some(0));
    }

    #[tokio::test]
    async fn retries_server_errors() {
        // Первый ответ 503, второй — успех. Сервер один на оба подключения.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for (i, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let (status, body) = if i == 0 {
                    ("503 Service Unavailable", &b""[..])
                } else {
                    ("200 OK", &b"ok"[..])
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body);
            }
        });
        let url = format!("http://{addr}/file");
        let dest = temp_file("retry.bin");
        let outcome = download(
            &url,
            &dest,
            Expected {
                size: Some(2),
                sha512: None,
                sha1: None,
                max_bytes: 1024,
            },
            "test",
            None,
            test_policy(),
        )
        .await
        .expect("retry succeeds");
        assert_eq!(outcome.bytes, 2);
        std::fs::remove_file(&dest).ok();
    }

    #[tokio::test]
    async fn failed_download_preserves_the_previous_destination() {
        let temp = crate::test_utils::TempDir::new("http-atomic-failure");
        let dest = temp.join("file.jar");
        std::fs::write(&dest, b"previous").expect("previous file");
        let url = spawn_server(b"new", 3, Duration::ZERO, "200 OK");

        let error = download(&url, &dest,
            Expected { size: Some(3), sha512: Some("00"), max_bytes: 1024, ..Expected::default() },
            "file.jar", None, test_policy()).await.expect_err("hash mismatch");

        assert_eq!(error.code(), "E_COMPAT_INVALID");
        assert_eq!(std::fs::read(&dest).expect("previous file must survive"), b"previous");
        assert_eq!(std::fs::read_dir(&temp).expect("temp").count(), 1);
    }

    #[tokio::test]
    async fn download_refuses_a_linked_destination_directory() {
        let temp = crate::test_utils::TempDir::new("http-linked-parent");
        let outside = crate::test_utils::TempDir::new("http-linked-outside");
        crate::test_utils::link_directory(&outside, &temp.join("downloads"));
        let url = spawn_server(b"new", 3, Duration::ZERO, "200 OK");

        let result = download(&url, &temp.join("downloads/file.jar"),
            Expected { max_bytes: 1024, ..Expected::default() },
            "file.jar", None, test_policy()).await;

        assert!(result.is_err(), "downloads must not follow a linked directory");
        assert!(!outside.join("file.jar").exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_download_removes_uncommitted_file_data() {
        let temp = crate::test_utils::TempDir::new("http-cancel-write");
        let dest = temp.join("file.jar");
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let url = serve_once(move |mut stream| {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\nfirst");
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started = std::sync::Mutex::new(Some(started_tx));
        let task_dest = dest.clone();
        let attempt = tokio::spawn(async move {
            let progress = move |_: u64, _: u64| {
                if let Some(sender) = started.lock().expect("started").take() {
                    let _ = sender.send(());
                }
            };
            download(&url, &task_dest, Expected { max_bytes: 4096, ..Expected::default() },
                "file.jar", Some(&progress), test_policy()).await
        });
        tokio::time::timeout(Duration::from_secs(5), started_rx).await.expect("download starts").expect("progress");

        attempt.abort();
        let _ = attempt.await;
        let _ = release_tx.send(());

        assert!(!dest.exists(), "an aborted download must not leave a partial destination");
        tokio::time::timeout(Duration::from_secs(2), async {
            while std::fs::read_dir(&temp).expect("temp").count() != 0 {
                tokio::task::yield_now().await;
            }
        }).await.expect("uncommitted temporary file must be removed");
    }

    #[tokio::test]
    async fn file_deadline_prevents_commit_after_a_slow_progress_callback() {
        let temp = crate::test_utils::TempDir::new("http-local-deadline");
        let dest = temp.join("file.jar");
        std::fs::write(&dest, b"previous").expect("previous file");
        let url = spawn_server(b"new", 3, Duration::ZERO, "200 OK");
        let progress = |_: u64, _: u64| std::thread::sleep(Duration::from_millis(150));

        let result = download(&url, &dest,
            Expected { max_bytes: 1024, ..Expected::default() }, "file.jar", Some(&progress),
            DownloadPolicy { file_timeout: Duration::from_millis(100), attempts: 1, ..test_policy() }).await;

        assert!(result.is_err(), "a file cannot be committed after its deadline");
        assert_eq!(std::fs::read(&dest).expect("previous file"), b"previous");
    }

    #[tokio::test]
    async fn retry_progress_does_not_count_partial_bytes_twice() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/file", listener.local_addr().expect("address"));
        std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let _ = stream.read(&mut [0u8; 4096]);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n");
                if attempt == 0 {
                    let _ = stream.write_all(b"01234");
                    std::thread::sleep(Duration::from_millis(80));
                } else {
                    let _ = stream.write_all(b"012");
                    std::thread::sleep(Duration::from_millis(80));
                    let _ = stream.write_all(b"3456789");
                }
            }
        });
        let temp = crate::test_utils::TempDir::new("http-retry-progress");
        let seen = std::sync::Mutex::new(Vec::new());
        let progress = |done: u64, _: u64| seen.lock().expect("progress").push(done);

        download(&url, &temp.join("file.jar"),
            Expected { size: Some(10), max_bytes: 1024, ..Expected::default() },
            "file.jar", Some(&progress),
            DownloadPolicy { low_speed_window: Duration::from_secs(2), ..test_policy() }).await.expect("retry");

        let seen = seen.into_inner().expect("progress");
        assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "progress rewound: {seen:?}");
        assert_eq!(seen.last(), Some(&10));
    }
}
