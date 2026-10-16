//! Поиск подходящей Java для прогона установщиков загрузчиков.
//!
//! Цепочка (без действий пользователя):
//! 1. Mojang-рантаймы в `<game_dir>/runtime` (их докачивает сам лаунчер);
//! 2. системная Java (`PATH`, `JAVA_HOME`);
//! 3. JRE из комплекта TLauncher (последний шанс; TLS у него капризный,
//!    см. мини-spike: дефолтный trust init падает, keystore при этом цел).
//! Старым Forge (`mc <= 1.16`) лучше Java 8–16 — новые JVM их часто роняют.

use crate::domain::{InstallError, LoaderKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Кандидат: путь к `java(.exe)` + мажорная версия + источник.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaCandidate {
    pub path: PathBuf,
    pub major: u64,
    pub source: JavaSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JavaSource {
    MojangRuntime,
    System,
    TlauncherBundled,
}

/// Требование к Java: минимальная мажорная версия; для старых Forge —
/// предпочтение 8–16 (`prefer_legacy`), иначе любая не ниже минимума.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JavaReq {
    pub min_major: u64,
    pub prefer_legacy: bool,
}

/// Мажорная версия из `java -version` (первая строка stdout/stderr):
/// `openjdk version "17.0.15"...` → 17; `java version "1.8.0_451"...` → 8.
pub fn parse_java_major(output: &str) -> Option<u64> {
    let line = output.lines().next()?;
    let quoted = line.split('"').nth(1)?;
    let mut parts = quoted.split('.');
    let first: u64 = parts.next()?.parse().ok()?;
    if first == 1 {
        // Старая схема `1.8.0_xxx`: мажор — второй компонент.
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// Выбор из кандидатов: фильтр по минимуму, затем источник, затем версия
/// (старшая из подходящих не нужна — берём младшую: меньше сюрпризов).
/// Legacy: сначала лучший из ≤16, иначе любой ≥ минимума.
pub fn pick_java(candidates: &[JavaCandidate], req: &JavaReq) -> Option<PathBuf> {
    let mut eligible: Vec<&JavaCandidate> = candidates
        .iter()
        .filter(|c| c.major >= req.min_major)
        .collect();
    if eligible.is_empty() {
        return None;
    }
    if req.prefer_legacy {
        let mut legacy: Vec<&JavaCandidate> =
            eligible.iter().filter(|c| c.major <= 16).copied().collect();
        if !legacy.is_empty() {
            legacy.sort_by_key(|c| (c.source, c.major));
            return legacy.into_iter().next().map(|c| c.path.clone());
        }
    }
    eligible.sort_by_key(|c| (c.source, c.major));
    eligible.into_iter().next().map(|c| c.path.clone())
}

/// Требование под пару (загрузчик, версия игры).
pub fn java_req_for(loader: LoaderKind, mc: &str) -> JavaReq {
    if loader == LoaderKind::Forge && mc_is_legacy(mc) {
        return JavaReq {
            min_major: 8,
            prefer_legacy: true,
        };
    }
    if loader == LoaderKind::NeoForge && mc_needs_java21(mc) {
        return JavaReq {
            min_major: 21,
            prefer_legacy: false,
        };
    }
    JavaReq {
        min_major: 17,
        prefer_legacy: false,
    }
}

/// Числовые компоненты версии (`"1.20.1"` → `[1, 20, 1]`, `"26.2"` → `[26, 2]`).
fn mc_components(mc: &str) -> Vec<u64> {
    mc.split('.')
        .map(|p| {
            p.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

/// Старый Minecraft (`1.x`, x ≤ 16): инсталлеры эпохи Java 8.
pub fn mc_is_legacy(mc: &str) -> bool {
    let c = mc_components(mc);
    c.first().copied().unwrap_or(0) == 1 && c.get(1).copied().unwrap_or(0) <= 16
}

/// Minecraft, которому для запуска нужна Java 21 (1.20.5+).
pub fn mc_needs_java21(mc: &str) -> bool {
    let c = mc_components(mc);
    let major = c.first().copied().unwrap_or(0);
    if major != 1 {
        return major > 1;
    }
    let minor = c.get(1).copied().unwrap_or(0);
    minor > 20 || (minor == 20 && c.get(2).copied().unwrap_or(0) >= 5)
}

/// Мажорная версия живой Java (запуск `java -version`, таймаут 15 с).
/// `None` — нет файла, не запустилась или повисла.
pub fn java_major(exe: &Path) -> Option<u64> {
    use std::sync::mpsc;
    let child = std::process::Command::new(exe)
        .arg("-version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = rx.recv_timeout(Duration::from_secs(15)).ok()?.ok()?;
    let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    parse_java_major(&text)
}

/// Mojang-рантаймы под game dir: `<runtime>/**/bin/java(.exe)`, строго
/// bounded (глубина 5, без full-disk scan — только известный корень).
fn discover_mojang_runtimes(game_dir: &Path) -> Vec<PathBuf> {
    let root = game_dir.join("runtime");
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .max_depth(5)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name != "java.exe" && name != "java" && name != "javaw.exe" {
            continue;
        }
        // Только `.../bin/java(.exe)`: мусор с тем же именем не ловим.
        let is_in_bin = entry
            .path()
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n.eq_ignore_ascii_case("bin"));
        if is_in_bin {
            out.push(entry.path().to_path_buf());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Системная Java: `PATH` + `JAVA_HOME`. Без реестра — этих двух хватает.
fn discover_system_java() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in ["java.exe", "java"] {
                let c = dir.join(name);
                if c.is_file() {
                    out.push(c);
                }
            }
        }
    }
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        for name in ["java.exe", "java"] {
            let c = Path::new(&home).join("bin").join(name);
            if c.is_file() {
                out.push(c);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// JRE из комплекта TLauncher: `<roaming>/.tlauncher/starter/*/bin/java(.exe)`.
/// Один уровень вглубь (`jre_default` и т.п.), без рекурсии.
fn discover_tlauncher_java(roaming: &Path) -> Vec<PathBuf> {
    let starter = roaming.join(".tlauncher").join("starter");
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&starter).map(|r| r.collect::<Vec<_>>());
    for entry in entries.unwrap_or_default().into_iter().filter_map(|e| e.ok()) {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // JRE лежит либо прямо в подпапке starter, либо на уровень глубже
        // (`jre_default/jre-21...-windows-x64/bin/java.exe`).
        for bin in [dir.join("bin")] {
            for name in ["java.exe", "java"] {
                let c = bin.join(name);
                if c.is_file() {
                    out.push(c);
                }
            }
        }
        if let Ok(inner) = std::fs::read_dir(&dir) {
            for sub in inner.filter_map(|e| e.ok()).map(|e| e.path()) {
                if !sub.is_dir() {
                    continue;
                }
                for name in ["java.exe", "java"] {
                    let c = sub.join("bin").join(name);
                    if c.is_file() {
                        out.push(c);
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Полный поиск по цепочке. `roaming` — `%APPDATA%` (для комплекта TL).
/// Возвращает путь подходящей Java или `NoJava`.
pub fn resolve_java(
    game_dir: &Path,
    roaming: Option<&Path>,
    req: &JavaReq,
) -> Result<PathBuf, InstallError> {
    let mut candidates = Vec::new();
    for path in discover_mojang_runtimes(game_dir) {
        if let Some(major) = java_major(&path) {
            candidates.push(JavaCandidate {
                path,
                major,
                source: JavaSource::MojangRuntime,
            });
        }
    }
    for path in discover_system_java() {
        if candidates.iter().any(|c: &JavaCandidate| c.path == path) {
            continue;
        }
        if let Some(major) = java_major(&path) {
            candidates.push(JavaCandidate {
                path,
                major,
                source: JavaSource::System,
            });
        }
    }
    if let Some(roaming) = roaming {
        for path in discover_tlauncher_java(roaming) {
            if candidates.iter().any(|c: &JavaCandidate| c.path == path) {
                continue;
            }
            if let Some(major) = java_major(&path) {
                candidates.push(JavaCandidate {
                    path,
                    major,
                    source: JavaSource::TlauncherBundled,
                });
            }
        }
    }
    pick_java(&candidates, req).ok_or_else(|| {
        InstallError::NoJava(format!(
            "нужна Java {}+ для подготовки версии, не найдена (проверены рантаймы игры, PATH/JAVA_HOME, комплект TLauncher)",
            req.min_major
        ))
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cand(source: JavaSource, major: u64) -> JavaCandidate {
        JavaCandidate {
            path: PathBuf::from(format!("/fake/{source:?}-{major}/bin/java")),
            major,
            source,
        }
    }

    #[test]
    fn parses_modern_java_version_output() {
        assert_eq!(
            parse_java_major("openjdk version \"17.0.15\" 2025-04-15 LTS\nOpenJDK Runtime Environment\n"),
            Some(17)
        );
        assert_eq!(
            parse_java_major("openjdk version \"21.0.11\" 2025-01-21 LTS\n"),
            Some(21)
        );
    }

    #[test]
    fn parses_legacy_1_8_scheme() {
        assert_eq!(
            parse_java_major("java version \"1.8.0_451\"\nJava(TM) SE Runtime Environment\n"),
            Some(8)
        );
        assert_eq!(parse_java_major("openjdk version \"11.0.22\"\n"), Some(11));
    }

    #[test]
    fn garbage_version_output_is_none() {
        assert_eq!(parse_java_major(""), None);
        assert_eq!(parse_java_major("hello world\n"), None);
        assert_eq!(parse_java_major("version\n"), None);
    }

    #[test]
    fn modern_pick_prefers_mojang_runtime_then_oldest() {
        let req = JavaReq {
            min_major: 17,
            prefer_legacy: false,
        };
        let cands = vec![
            cand(JavaSource::System, 21),
            cand(JavaSource::MojangRuntime, 21),
            cand(JavaSource::MojangRuntime, 17),
        ];
        assert_eq!(
            pick_java(&cands, &req),
            Some(PathBuf::from("/fake/MojangRuntime-17/bin/java"))
        );
    }

    #[test]
    fn below_minimum_is_filtered_out() {
        let req = JavaReq {
            min_major: 17,
            prefer_legacy: false,
        };
        let cands = vec![cand(JavaSource::System, 8), cand(JavaSource::System, 11)];
        assert_eq!(pick_java(&cands, &req), None);
        assert_eq!(pick_java(&[], &req), None);
    }

    #[test]
    fn legacy_prefers_oldest_within_8_to_16() {
        let req = JavaReq {
            min_major: 8,
            prefer_legacy: true,
        };
        let cands = vec![
            cand(JavaSource::System, 21),
            cand(JavaSource::System, 8),
            cand(JavaSource::MojangRuntime, 17),
        ];
        assert_eq!(
            pick_java(&cands, &req),
            Some(PathBuf::from("/fake/System-8/bin/java"))
        );
    }

    #[test]
    fn legacy_falls_back_to_modern_when_no_old_java() {
        let req = JavaReq {
            min_major: 8,
            prefer_legacy: true,
        };
        let cands = vec![cand(JavaSource::System, 17)];
        assert_eq!(
            pick_java(&cands, &req),
            Some(PathBuf::from("/fake/System-17/bin/java"))
        );
    }

    #[test]
    fn tlauncher_bundled_is_last_resort() {
        let req = JavaReq {
            min_major: 17,
            prefer_legacy: false,
        };
        let cands = vec![
            cand(JavaSource::TlauncherBundled, 21),
            cand(JavaSource::System, 17),
        ];
        assert_eq!(
            pick_java(&cands, &req),
            Some(PathBuf::from("/fake/System-17/bin/java"))
        );
    }

    #[test]
    fn old_forge_requests_legacy_java() {
        let req = java_req_for(LoaderKind::Forge, "1.16.5");
        assert_eq!(req.min_major, 8);
        assert!(req.prefer_legacy);
    }

    #[test]
    fn modern_loaders_request_java_17() {
        for (loader, mc) in [
            (LoaderKind::Forge, "1.20.1"),
            (LoaderKind::Fabric, "1.20.1"),
            (LoaderKind::Quilt, "1.20.1"),
            (LoaderKind::NeoForge, "1.20.1"),
        ] {
            let req = java_req_for(loader, mc);
            assert_eq!(req.min_major, 17, "{loader:?} {mc}");
            assert!(!req.prefer_legacy);
        }
    }

    #[test]
    fn neoforge_on_java21_minecraft_requests_java21() {
        for mc in ["1.20.5", "1.20.6", "1.21", "1.21.1", "26.1"] {
            let req = java_req_for(LoaderKind::NeoForge, mc);
            assert_eq!(req.min_major, 21, "{mc}");
        }
        let req = java_req_for(LoaderKind::NeoForge, "1.20.4");
        assert_eq!(req.min_major, 17);
    }

    #[test]
    fn mc_version_classification() {
        assert!(mc_is_legacy("1.16.5"));
        assert!(mc_is_legacy("1.12.2"));
        assert!(!mc_is_legacy("1.17"));
        assert!(!mc_is_legacy("1.20.1"));
        assert!(!mc_needs_java21("1.20.4"));
        assert!(mc_needs_java21("1.20.5"));
        assert!(mc_needs_java21("1.21.1"));
        assert!(mc_needs_java21("26.2"));
        assert!(!mc_needs_java21("1.16.5"));
    }
}
