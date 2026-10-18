//! Поиск подходящей Java для прогона установщиков загрузчиков.
//!
//! Где именно лежит Java — **свойство дистрибуции лаунчера**, а не единая
//! договорённость. Standalone-лаунчер кладёт рантаймы в `<game_dir>/runtime`,
//! а Store-сборка — в `%LOCALAPPDATA%\Packages\<PFN>\LocalCache\Local\runtime`,
//! и эти пути не пересекаются. Поэтому корни задаёт вызывающий адаптер
//! ([`JavaRoots`]), а не этот модуль: иначе «однажды настроенный» путь молча
//! перестаёт работать на другой сборке того же лаунчера.
//!
//! Цепочка (без действий пользователя):
//! 1. рантаймы самого лаунчера — совместимы с его версиями игры, приоритет
//!    выше системной;
//! 2. системная Java (`PATH`, `JAVA_HOME`);
//! 3. Java из комплекта лаунчера (`starter/*` у TLauncher, `jre/<arch>` у
//!    Legacy Launcher).
//!
//! Каждый кандидат проверяется запуском `java -version` ([`java_major`]),
//! поэтому в выборку попадают только реально рабочие JVM.
//! Старым Forge (`mc <= 1.16`) лучше Java 8–16 — новые JVM их часто роняют.

use crate::domain::{InstallError, LoaderKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Где искать Java для конкретного лаунчера. Адаптер знает раскладку своей
/// дистрибуции и заполняет поля сам; ничего не зашито здесь.
#[derive(Debug, Default, Clone)]
pub struct JavaRoots {
    /// Рантаймы, которые докачивает сам лаунчер (Mojang). Приоритет выше
    /// системной: они гарантированно совместимы с его версиями игры.
    pub mojang: Vec<PathBuf>,
    /// Java из комплекта лаунчера: TLauncher `starter`, Legacy `jre/<arch>`.
    pub bundled: Vec<PathBuf>,
}

/// Глубины обхода. 6 покрывает вложенность Store-рантайма
/// (`runtime/java-runtime-delta/windows-x64/java-runtime-delta/bin/java.exe`),
/// 5 — комплекты лаунчеров. Это bounded-обход известных корней, не скан диска.
const MOJANG_RUNTIME_DEPTH: usize = 6;
const BUNDLED_JAVA_DEPTH: usize = 5;

/// Максимум корней в диагностике `NoJava`: список — для человека, а не лог.
const MAX_REPORTED_ROOTS: usize = 6;

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
    /// Комплект лаунчера: TLauncher / Legacy Launcher / Store-лаунчер.
    Bundled,
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

/// Java в заданных корнях: `**/bin/java(.exe)`, строго bounded — только
/// переданные корни, никакого полного сканирования диска.
fn discover_java_in(roots: &[PathBuf], max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(root)
            .max_depth(max_depth)
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
            if entry
                .path()
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n.eq_ignore_ascii_case("bin"))
            {
                out.push(entry.path().to_path_buf());
            }
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

/// Java из комплекта лаунчера: обход переданных корней.
fn discover_bundled_java(roots: &[PathBuf]) -> Vec<PathBuf> {
    discover_java_in(roots, BUNDLED_JAVA_DEPTH)
}

/// Диагностика для `NoJava`: чего не хватило, что нашли и где смотрели.
/// Пользователь читает это в «Подробнее», поэтому конкретные пути ценнее
/// перечисления категорий.
fn no_java_report(candidates: &[JavaCandidate], roots: &JavaRoots, req: &JavaReq) -> String {
    let mut report =
        format!("нужна Java {}+ для подготовки версии игры", req.min_major);

    let too_old: Vec<String> = candidates
        .iter()
        .filter(|c| c.major < req.min_major)
        .map(|c| format!("Java {} ({})", c.major, c.path.display()))
        .collect();
    if !too_old.is_empty() {
        report.push_str(&format!(
            "; нашена, но старее нужной: {}",
            too_old.join(", ")
        ));
    }

    let probed: Vec<String> = roots
        .mojang
        .iter()
        .chain(&roots.bundled)
        .filter(|p| p.is_dir())
        .take(MAX_REPORTED_ROOTS)
        .map(|p| p.display().to_string())
        .collect();
    if probed.is_empty() {
        report.push_str("; папок с рантаймами лаунчера не найдено");
    } else {
        report.push_str(&format!("; проверенные папки: {}", probed.join(", ")));
    }
    report.push_str("; системная Java: PATH и JAVA_HOME");
    report
}

/// Полный поиск по цепочке. `roots` задаёт вызывающий адаптер — расположение
/// рантаймов зависит от дистрибуции лаунчера. Возвращает путь подходящей
/// Java или `NoJava` с диагностикой.
pub fn resolve_java(roots: &JavaRoots, req: &JavaReq) -> Result<PathBuf, InstallError> {
    let mut candidates: Vec<JavaCandidate> = Vec::new();
    let push = |path: PathBuf, source: JavaSource, candidates: &mut Vec<JavaCandidate>| {
        if candidates.iter().any(|c: &JavaCandidate| c.path == path) {
            return;
        }
        // Кандидат в выборку попадает только если JVM реально стартует.
        if let Some(major) = java_major(&path) {
            candidates.push(JavaCandidate {
                path,
                major,
                source,
            });
        }
    };

    for path in discover_java_in(&roots.mojang, MOJANG_RUNTIME_DEPTH) {
        push(path, JavaSource::MojangRuntime, &mut candidates);
    }
    for path in discover_system_java() {
        push(path, JavaSource::System, &mut candidates);
    }
    for path in discover_bundled_java(&roots.bundled) {
        push(path, JavaSource::Bundled, &mut candidates);
    }

    pick_java(&candidates, req)
        .ok_or_else(|| InstallError::NoJava(no_java_report(&candidates, roots, req)))
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
    fn bundled_java_is_last_resort() {
        let req = JavaReq {
            min_major: 17,
            prefer_legacy: false,
        };
        let cands = vec![
            cand(JavaSource::Bundled, 21),
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

    /// Раскладка Store-рантайма глубже, чем у Standalone: считываем её
    /// в глубину, иначе `java.exe` выпадает из обхода и мы возвращаемся к
    /// той же ошибке, что чинили.
    #[test]
    fn finds_java_in_nested_mojang_runtime_layout() {
        let root = std::env::temp_dir().join(format!("km-java-nested-{}", std::process::id()));
        let runtime = root
            .join("java-runtime-delta")
            .join("windows-x64")
            .join("java-runtime-delta");
        std::fs::create_dir_all(runtime.join("bin")).expect("mkdir");
        let bin = runtime.join("bin");
        for name in ["java.exe", "javaw.exe"] {
            std::fs::write(bin.join(name), b"not a real jvm").expect("write");
        }
        // Мусор с тем же именем вне `bin/` ловиться не должен.
        std::fs::create_dir_all(runtime.join("other")).expect("mkdir");
        std::fs::write(runtime.join("other").join("java.exe"), b"decoy").expect("write");

        let found = discover_java_in(std::slice::from_ref(&root), MOJANG_RUNTIME_DEPTH);
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(
            found,
            vec![bin.join("java.exe"), bin.join("javaw.exe")],
            "Store-раскладка должна находиться целиком, мусор вне bin — нет",
        );
    }

    #[test]
    fn missing_root_yields_no_candidates() {
        let absent = std::env::temp_dir().join("km-java-definitely-absent-root");
        assert!(discover_java_in(&[absent], MOJANG_RUNTIME_DEPTH).is_empty());
    }

    /// Диагностика обязана называть версию найденной, но слишком старой Java:
    /// «не нашли Java» без версии бесполезно — пользователь не знает, что у
    /// него уже стоит.
    #[test]
    fn no_java_report_names_too_old_candidates_and_probed_roots() {
        let root = std::env::temp_dir().join(format!("km-java-report-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("mkdir");
        let candidates = vec![JavaCandidate {
            path: PathBuf::from("C:/Program Files/Java/jre1.8.0_461/bin/java.exe"),
            major: 8,
            source: JavaSource::System,
        }];
        let roots = JavaRoots {
            mojang: vec![root.clone(), root.join("absent")],
            bundled: Vec::new(),
        };
        let req = JavaReq {
            min_major: 17,
            prefer_legacy: false,
        };
        let report = no_java_report(&candidates, &roots, &req);
        std::fs::remove_dir_all(&root).ok();

        assert!(report.contains("Java 17+"), "{report}");
        assert!(report.contains("Java 8"), "назван найденный старый: {report}");
        assert!(report.contains("jre1.8.0_461"), "назван путь: {report}");
        assert!(report.contains(&root.display().to_string()), "назван корень: {report}");
        assert!(!report.contains("absent"), "несуществующие корни не вводят в заблуждение: {report}");
    }

    #[test]
    fn no_java_report_says_when_no_runtime_folders_exist() {
        let roots = JavaRoots::default();
        let req = JavaReq {
            min_major: 17,
            prefer_legacy: false,
        };
        let report = no_java_report(&[], &roots, &req);
        assert!(report.contains("рантаймами"), "{report}");
    }
}
