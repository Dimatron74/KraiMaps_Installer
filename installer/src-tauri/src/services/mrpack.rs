//! Установка входного `.mrpack`: проверенный индекс, скачивание файлов
//! в приватный staging и последующий деплой финального client-содержимого.

use crate::adapters::modrinth_pack::{self, ParsedPack};
use crate::domain::{InstallError, LoaderKind};
use crate::services::compat_temp::CompatLease;
use crate::services::work_budget::WorkBudget;
use crate::services::{deploy, http};
use futures_util::{StreamExt, TryStreamExt};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const DOWNLOAD_CONCURRENCY: usize = 4;

#[derive(Debug, Clone)]
pub struct MrpackLayout {
    pub world_prefix: String,
}

struct Inspection {
    parsed: ParsedPack,
    bytes: Vec<u8>,
    final_files: Vec<(String, u64)>,
    world_prefix: String,
    overrides_bytes: u64,
}

pub fn is_mrpack(path: &Path) -> bool {
    std::fs::File::open(path)
        .ok()
        .and_then(|file| zip::ZipArchive::new(file).ok())
        .is_some_and(|mut zip| zip.by_name("modrinth.index.json").is_ok())
}

fn inspect(path: &Path, expected_mc: Option<&str>) -> Result<Inspection, InstallError> {
    let budget = WorkBudget::default();
    let bytes = std::fs::read(path)?;
    let parsed = modrinth_pack::parse_index_for_install_with_budget(
        &modrinth_pack::read_index_with_budget(&bytes, &budget)?,
        expected_mc,
        &budget,
    )?;
    let overrides = modrinth_pack::final_override_entries_with_budget(&bytes, &budget)?;
    let mut final_files: BTreeMap<String, (String, u64)> = BTreeMap::new();
    for file in &parsed.files {
        final_files.insert(
            modrinth_pack::path_key(&file.path),
            (file.path.clone(), file.size),
        );
    }
    let mut overrides_bytes = 0u64;
    for entry in modrinth_pack::override_entries_for_install(&bytes, &budget)? {
        overrides_bytes = overrides_bytes.saturating_add(entry.size);
    }
    for entry in overrides {
        final_files.insert(
            modrinth_pack::path_key(&entry.path),
            (entry.path, entry.size),
        );
    }
    let final_files: Vec<_> = final_files.into_values().collect();
    let worlds: Vec<_> = final_files
        .iter()
        .filter(|(path, _)| path == "level.dat" || path.ends_with("/level.dat"))
        .collect();
    if worlds.is_empty() {
        return Err(InstallError::NoLevelDat);
    }
    if worlds.len() > 1 {
        let mut roots: Vec<String> = worlds.iter().map(|(path, _)| parent_prefix(path)).collect();
        roots.sort();
        return Err(InstallError::MultipleWorlds(roots));
    }
    let world_prefix = parent_prefix(&worlds[0].0);
    Ok(Inspection {
        parsed,
        bytes,
        final_files,
        world_prefix,
        overrides_bytes,
    })
}

fn parent_prefix(path: &str) -> String {
    path.rfind('/')
        .map(|index| path[..=index].to_string())
        .unwrap_or_default()
}

pub fn stage_totals(path: &Path) -> Result<deploy::StageTotals, InstallError> {
    let inspected = inspect(path, None)?;
    let mut world = 0u64;
    let mut layers = 0u64;
    for (path, size) in &inspected.final_files {
        if path.starts_with(&inspected.world_prefix) {
            world = world.saturating_add(*size);
        } else {
            layers = layers.saturating_add(*size);
        }
    }
    Ok(deploy::StageTotals {
        world,
        layers,
        compat: Some(inspected.parsed.total_bytes),
        compat_apply: Some(inspected.overrides_bytes),
        ..Default::default()
    })
}

pub async fn stage_mrpack(
    path: &Path,
    expected_mc: Option<&str>,
    expected_loader: LoaderKind,
    expected_loader_version: Option<&str>,
    staging: &CompatLease,
    progress: deploy::ProgressFn,
    budget: &WorkBudget,
) -> Result<MrpackLayout, InstallError> {
    let inspected = inspect(path, expected_mc)?;
    let loader_version = (!inspected.parsed.loader_version.is_empty())
        .then_some(inspected.parsed.loader_version.as_str());
    if inspected.parsed.loader != expected_loader || loader_version != expected_loader_version {
        return Err(InstallError::CompatInvalid(format!(
            "сборка требует {:?} {}; цель: {:?} {}",
            inspected.parsed.loader,
            loader_version.unwrap_or("—"),
            expected_loader,
            expected_loader_version.unwrap_or("—"),
        )));
    }
    download_files(&inspected.parsed, staging, &progress, budget).await?;
    progress(deploy::StageEvent::new(
        "compat-apply",
        0,
        inspected.overrides_bytes,
        "Файлы сборки".into(),
    ));
    let (bytes, worker_stage, worker_budget) =
        (inspected.bytes.clone(), staging.clone(), budget.clone());
    budget
        .blocking(move || {
            modrinth_pack::extract_overrides_with_budget(&bytes, &worker_stage, &worker_budget)
        })
        .await
        .map_err(|error| InstallError::CompatApply(error.to_string()))??;
    progress(deploy::StageEvent::new(
        "compat-apply",
        inspected.overrides_bytes,
        inspected.overrides_bytes,
        "Файлы сборки".into(),
    ));
    let level_dat = staging.join(&inspected.world_prefix).join("level.dat");
    if !level_dat.is_file() {
        return Err(InstallError::ZipCorrupt(
            "мир сборки распаковался без level.dat".into(),
        ));
    }
    Ok(MrpackLayout {
        world_prefix: inspected.world_prefix,
    })
}

async fn download_files(
    parsed: &ParsedPack,
    staging: &CompatLease,
    progress: &deploy::ProgressFn,
    budget: &WorkBudget,
) -> Result<(), InstallError> {
    let total = parsed.total_bytes;
    let display_name = parsed.name.as_deref().unwrap_or("Сборка карты").to_string();
    progress(deploy::StageEvent::new(
        "compat-download",
        0,
        total,
        display_name.clone(),
    ));
    let stage_done = Arc::new(AtomicU64::new(0));
    futures_util::stream::iter(parsed.files.iter().cloned())
        .map(|file| {
            let stage_done = stage_done.clone();
            let staging = staging.clone();
            let display_name = display_name.clone();
            async move {
                let dest = staging
                    .file(Path::new(&file.path))
                    .map_err(|error| InstallError::CompatApply(error.to_string()))?;
                let file_done = AtomicU64::new(0);
                let callback = |bytes: u64, _total: u64| {
                    let previous = file_done.swap(bytes, Ordering::Relaxed);
                    let delta = bytes.saturating_sub(previous);
                    let overall = stage_done.fetch_add(delta, Ordering::Relaxed) + delta;
                    progress(deploy::StageEvent::new(
                        "compat-download",
                        overall,
                        total,
                        format!("{display_name} · {}", file.path),
                    ));
                };
                http::download_owned(
                    &file.url,
                    &dest,
                    http::Expected {
                        size: Some(file.size),
                        sha512: Some(&file.sha512),
                        sha1: file.sha1.as_deref(),
                        max_bytes: modrinth_pack::MAX_FILE_BYTES,
                    },
                    &file.path,
                    Some(&callback),
                    http::DownloadPolicy::default(),
                    budget,
                )
                .await
                .map(|_| dest.keep())
            }
        })
        .buffer_unordered(DOWNLOAD_CONCURRENCY)
        .try_collect::<Vec<()>>()
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::work_budget::WorkBudget;
    use crate::test_utils::{TempDir, compat_root, serve_http_once, write_zip};
    use sha2::Digest;

    fn index(url: &str, body: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "name": "Map Pack",
            "files": [{
                "path": "mods/sodium.jar",
                "hashes": { "sha512": crate::services::http::hex(sha2::Sha512::digest(body)) },
                "downloads": [url],
                "fileSize": body.len(),
            }],
            "dependencies": { "minecraft": "1.20.6", "fabric-loader": "0.19.3" },
        }))
        .expect("index")
    }

    #[tokio::test]
    async fn stage_downloads_index_files_and_applies_overrides() {
        let source = TempDir::new("mrpack-stage-source");
        let parent = TempDir::new("mrpack-stage-temp");
        let temp = compat_root(&parent);
        let mod_body = b"verified-mod".to_vec();
        let (url, server) = serve_http_once(mod_body.clone(), std::time::Duration::ZERO).await;
        let pack = source.join("map.mrpack");
        write_zip(
            &pack,
            &[
                ("modrinth.index.json", &index(&url, &mod_body)),
                ("overrides/saves/World/level.dat", b"world"),
                ("overrides/options.txt", b"options"),
                ("client-overrides/options.txt", b"client-options"),
            ],
        );
        let staging = temp.staging().expect("staging");
        let progress: deploy::ProgressFn = std::sync::Arc::new(|_| {});

        let layout = stage_mrpack(
            &pack,
            Some("1.20.6"),
            crate::domain::LoaderKind::Fabric,
            Some("0.19.3"),
            &staging,
            progress,
            &WorkBudget::default(),
        )
        .await
        .expect("stage mrpack");
        server.await.expect("server");

        assert_eq!(layout.world_prefix, "saves/World/");
        assert_eq!(
            std::fs::read(staging.join("mods/sodium.jar")).expect("mod"),
            mod_body
        );
        assert_eq!(
            std::fs::read(staging.join("saves/World/level.dat")).expect("world"),
            b"world"
        );
        assert_eq!(
            std::fs::read(staging.join("options.txt")).expect("options"),
            b"client-options"
        );
    }

    #[test]
    fn totals_include_download_staging_world_and_content() {
        let source = TempDir::new("mrpack-stage-totals");
        let pack = source.join("map.mrpack");
        write_zip(
            &pack,
            &[
                (
                    "modrinth.index.json",
                    &index("https://cdn.modrinth.com/mod.jar", b"12345678"),
                ),
                ("overrides/saves/World/level.dat", b"world"),
                ("overrides/options.txt", b"opts"),
            ],
        );

        let totals = stage_totals(&pack).expect("totals");
        assert_eq!(totals.compat, Some(8));
        assert_eq!(totals.compat_apply, Some(9));
        assert_eq!(totals.world, 5);
        assert_eq!(totals.layers, 12);
    }
}
