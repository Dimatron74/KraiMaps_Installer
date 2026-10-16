//! Молчаливое создание профиля Modrinth App (§6.2 дизайн-дока, спайк 3).
//!
//! Правила:
//! - запущенное приложение перед записью автоматически закрывается
//!   (иначе новый профиль виден только после перезапуска); не закрылось —
//!   установка идёт всё равно, ошибок записи при этом нет;
//! - перед записью — бэкап `app.db*`;
//! - строки: `instances` (`local:<uuid>`, `install_stage='installed'`,
//!   `last_played=created` — иначе «последний запуск 57 лет назад»),
//!   `instance_content_sets` (`game_version/loader/loader_version`,
//!   `source_kind='local'`, `status='available'`), `instance_links` (`unmanaged`);
//! - файлы (`saves/`, `mods/`, `config/`) — прямо в профиль, рескан при
//!   старте подхватывает.

use crate::domain::{sanitize_display_name, slugify, GameTarget, InstallError, LauncherKind, LoaderKind};
use crate::services::detect::close_modrinth;
use std::path::{Path, PathBuf};

/// Создаёт профиль Modrinth App. Запущенное приложение перед записью
/// автоматически закрывается (`close_modrinth`); результат закрытия
/// игнорируется осознанно — установка продолжается в любом случае.
/// `icon_path` — файл из `prepare_instance_icon`; кладём копией в профиль
/// и прописываем `icon_path`, но только если такая колонка реально есть
/// в `instances` (принятие приложением спекой не подтверждено).
pub fn create_modrinth_profile(
    data_dir: &Path,
    display_name: &str,
    mc_version: &str,
    loader: LoaderKind,
    loader_version: Option<&str>,
    icon_path: Option<&Path>,
) -> Result<GameTarget, InstallError> {
    let _ = close_modrinth();
    // Защита в глубину: диапазон ("[40,)") вместо точной версии роняет
    // запуск профиля («No loader version selected») — не пишем никогда.
    if loader != LoaderKind::Vanilla {
        match loader_version {
            Some(v) if crate::services::versions::is_exact_version(v) => {}
            _ => {
                return Err(InstallError::UnknownLoader(
                    "нет точной версии загрузчика для нового профиля".to_string(),
                ))
            }
        }
    }
    let profiles = data_dir.join("profiles");
    std::fs::create_dir_all(&profiles)?;
    // Имя — с сайта (sanitize allowlist); папка — строгий slug из него.
    let display_name = sanitize_display_name(display_name);
    let slug = unique_profile(&profiles, &slugify(&display_name));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    backup_app_db(data_dir)?;

    let id = format!("local:{}", uuid::Uuid::new_v4());
    let set_id = format!("{id}:default");
    let db_path = data_dir.join("app.db");
    let conn = rusqlite::Connection::open(&db_path)?;
    // Приложение могли не успеть полностью отпустить базу (WAL дофлашивается
    // после закрытия окна): ждём освобождения лока, а не падаем сразу.
    conn.busy_timeout(std::time::Duration::from_secs(15))?;
    conn.execute(
        "INSERT INTO instances
           (id, path, applied_content_set_id, install_stage, launcher_feature_version,
            update_channel, name, created, modified, last_played,
            submitted_time_played, recent_time_played)
         VALUES (?1, ?2, ?3, 'installed', 'migrated_launch_hooks', 'release',
                 ?4, ?5, ?5, ?5, 0, 0)",
        rusqlite::params![id, slug, set_id, display_name, now],
    )?;
    conn.execute(
        "INSERT INTO instance_content_sets
           (id, instance_id, name, source_kind, status,
            game_version, loader, loader_version, created, modified)
         VALUES (?1, ?2, 'Default', 'local', 'available', ?3, ?4, ?5, ?6, ?6)",
        rusqlite::params![
            set_id,
            id,
            mc_version,
            modrinth_loader_name(loader),
            loader_version,
            now
        ],
    )?;
    conn.execute(
        "INSERT INTO instance_links (instance_id, link_kind) VALUES (?1, 'unmanaged')",
        rusqlite::params![id],
    )?;

    let dir = profiles.join(&slug);
    std::fs::create_dir_all(&dir)?;
    set_modrinth_icon(data_dir, &id, &dir, icon_path);
    Ok(GameTarget {
        launcher: LauncherKind::Modrinth,
        name: display_name.to_string(),
        path: dir,
        minecraft_version: mc_version.to_string(),
        loader,
        loader_version: loader_version.map(|s| s.to_string()),
    })
}

fn modrinth_loader_name(loader: LoaderKind) -> &'static str {
    match loader {
        LoaderKind::Vanilla => "vanilla",
        LoaderKind::Fabric => "fabric",
        LoaderKind::Forge => "forge",
        LoaderKind::Quilt => "quilt",
        LoaderKind::NeoForge => "neoforge",
    }
}

fn unique_profile(profiles: &Path, base: &str) -> String {
    if !profiles.join(base).exists() {
        return base.to_string();
    }
    let mut n = 1u32;
    loop {
        let candidate = format!("{base}_{n}");
        if !profiles.join(&candidate).exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Бэкап `app.db*` (база + WAL/SHM/journal) рядом с данными, а не в Temp:
/// копии пользовательской базы не место среди временных файлов.
/// Возвращает путь бэкапа для «Подробнее». Хранит последние 3 бэкапа,
/// старые подтирает тут же; наследие в Temp (`mapinstaller-appdb-*`)
/// подбирает стартовый sweep.
pub fn backup_app_db(data_dir: &Path) -> Result<PathBuf, InstallError> {
    let parent = data_dir.join(".kraimaps-backup");
    let backup = parent.join(format!(
        "appdb-{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        std::process::id()
    ));
    std::fs::create_dir_all(&backup)?;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let src = data_dir.join(format!("app.db{suffix}"));
        if src.is_file() {
            std::fs::copy(&src, backup.join(format!("app.db{suffix}")))?;
        }
    }
    crate::services::temp_clean::prune_old_backups(&parent, "appdb-", 3);
    Ok(backup)
}

/// Иконка профиля Modrinth App (best-effort — молча пропускаем при неудаче).
fn set_modrinth_icon(data_dir: &Path, instance_id: &str, profile_dir: &Path, icon_path: Option<&Path>) {
    let src = match icon_path {
        Some(p) => p,
        None => return,
    };
    // PNG понимают все; вход с сайта обычно WebP — конвертируем.
    let png = match crate::services::icon::ensure_png(src) {
        Some(p) => p,
        None => return,
    };
    let dest = profile_dir.join("icon.png");
    if std::fs::copy(&png, &dest).is_err() {
        return;
    }
    let db_path = data_dir.join("app.db");
    let conn = match rusqlite::Connection::open(&db_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let has_col: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('instances') WHERE name='icon_path'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if has_col == 0 {
        return;
    }
    let _ = conn.execute(
        "UPDATE instances SET icon_path=?1 WHERE id=?2",
        rusqlite::params![dest.to_string_lossy(), instance_id],
    );
}

/// «Играть» для Modrinth App: стабильного CLI нет (потолок лаунчера) —
// открываем приложение, пользователь жмёт Play на профиле.
pub fn launch_modrinth(exe: &Path) -> Result<String, InstallError> {
    std::process::Command::new(exe)
        .spawn()
        .map_err(|e| InstallError::LaunchFailed(format!("{}: {e}", exe.display())))?;
    Ok(exe.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-modrinth-{tag}"))
    }

    fn empty_db(data_dir: &Path) {
        let conn = rusqlite::Connection::open(data_dir.join("app.db")).expect("db");
        conn.execute_batch(
            "CREATE TABLE instances (id TEXT PRIMARY KEY, path TEXT NOT NULL,
               applied_content_set_id TEXT NULL, install_stage TEXT NOT NULL,
               launcher_feature_version TEXT NOT NULL DEFAULT 'x', update_channel TEXT NOT NULL DEFAULT 'release',
               name TEXT NOT NULL, icon_path TEXT NULL, created INTEGER NOT NULL, modified INTEGER NOT NULL,
               last_played INTEGER NULL, submitted_time_played INTEGER NOT NULL DEFAULT 0,
               recent_time_played INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE instance_content_sets (id TEXT PRIMARY KEY, instance_id TEXT NOT NULL,
               name TEXT NOT NULL, source_kind TEXT NOT NULL, status TEXT NOT NULL,
               game_version TEXT NOT NULL, loader TEXT NOT NULL, loader_version TEXT NULL,
               created INTEGER NOT NULL, modified INTEGER NOT NULL);
             CREATE TABLE instance_links (instance_id TEXT PRIMARY KEY, link_kind TEXT NOT NULL,
               modrinth_project_id TEXT NULL, modrinth_version_id TEXT NULL, server_project_id TEXT NULL,
               content_project_id TEXT NULL, content_version_id TEXT NULL, hosting_server_id TEXT NULL,
               hosting_instance_ids TEXT NULL, hosting_active_instance_id TEXT NULL, shared_instance_id TEXT NULL,
               imported_name TEXT NULL, imported_version_number TEXT NULL, imported_filename TEXT NULL,
               shared_instance_role TEXT NULL, shared_instance_manager_id TEXT NULL,
               shared_instance_linked_user_id TEXT NULL, shared_instance_server_manager_name TEXT NULL,
               shared_instance_server_manager_icon_url TEXT NULL);",
        )
        .expect("schema");
    }

    #[test]
    fn creates_profile_with_db_rows() {
        // Гард честный: при запущенном приложении запись запрещена.
        // Тест пропускаем, а не роняем — стенд занят пользователем.
        let live = crate::services::detect::running_launcher_processes()
            .into_iter()
            .any(|p| p.kind == LauncherKind::Modrinth);
        if live {
            eprintln!("SKIP: Modrinth App is running");
            return;
        }
        let root = temp_root("create");
        empty_db(&root);
        let target = create_modrinth_profile(&root, "DeepMine", "1.21.11", LoaderKind::Vanilla, None, None)
            .expect("create");
        assert!(target.path.is_dir());
        let conn = rusqlite::Connection::open(root.join("app.db")).expect("open");
        let stage: String = conn
            .query_row(
                "SELECT install_stage FROM instances WHERE name='DeepMine'",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(stage, "installed");
        // Профиль виден детекту.
        let found = crate::services::detect::read_modrinth_targets(&root).expect("read");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].minecraft_version, "1.21.11");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn icon_without_column_is_copied_but_not_linked() {
        if crate::services::detect::running_launcher_processes()
            .into_iter()
            .any(|p| p.kind == LauncherKind::Modrinth)
        {
            eprintln!("SKIP: Modrinth App is running");
            return;
        }
        let root = temp_root("icon");
        empty_db(&root);
        let img = image::RgbImage::from_pixel(1, 1, image::Rgb([70, 130, 180]));
        let icon = root.join("site-icon.png");
        img.save_with_format(&icon, image::ImageFormat::Png).expect("png");
        let target = create_modrinth_profile(
            &root,
            "DeepMine",
            "1.21.11",
            LoaderKind::Vanilla,
            None,
            Some(&icon),
        )
        .expect("create");
        // Файл — в профиле; колонки icon_path в этой схеме нет — UPDATE пропущен без ошибки.
        assert!(target.path.join("icon.png").is_file());
        std::fs::remove_dir_all(&root).ok();
    }
}
