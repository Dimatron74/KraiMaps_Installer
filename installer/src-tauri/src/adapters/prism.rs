//! Молчаливое создание инстанса Prism (§6.1 дизайн-дока, рецепт спайка 2).
//!
//! - `instances/<Slug>/` (конфликт → `_1`, `_2`...);
//! - `instance.cfg`: `ConfigVersion=1.3`, `name`, `InstanceType=OneSix`,
//!   `ManagedPack=false` (иначе Prism сочтёт инстанс управляемым паком);
//! - `mmc-pack.json`: `formatVersion: 1`, компоненты `net.minecraft`
//!   (+ загрузчик). Jar и библиотеки Prism докачивает сам при запуске.
//! - `minecraft/saves/` создаём мы при деплое мира.

use crate::domain::{sanitize_display_name, slugify, GameTarget, InstallError, LauncherKind, LoaderKind};
use std::path::Path;

/// Создаёт инстанс Prism. `loader_version` — точная версия (см. `versions`).
/// `icon_path` — файл иконки из `prepare_instance_icon`; ставим только PNG
/// (`icons/<slug>.png` + `iconKey`, механизм спеки): конвертера без image-
/// зависимости нет, не-PNG молча пропускаем, а не ломаем инстанс.
pub fn create_prism_instance(
    data_dir: &Path,
    display_name: &str,
    mc_version: &str,
    loader: LoaderKind,
    loader_version: Option<&str>,
    icon_path: Option<&Path>,
) -> Result<GameTarget, InstallError> {
    let instances = data_dir.join("instances");
    std::fs::create_dir_all(&instances)?;
    // Имя — с сайта (sanitize allowlist); папка — строгий slug из него.
    let display_name = sanitize_display_name(display_name);
    let slug = unique_slug(&instances, &slugify(&display_name));
    let dir = instances.join(&slug);

    // Защита в глубину: диапазон ("[40,)") или мусор вместо точной версии
    // роняет обновление инстанса в Prism — такие значения не пишем никогда.
    if loader != LoaderKind::Vanilla {
        match loader_version {
            Some(v) if crate::services::versions::is_exact_version(v) => {}
            _ => {
                return Err(InstallError::UnknownLoader(
                    "нет точной версии загрузчика для нового инстанса".to_string(),
                ))
            }
        }
    }

    let mut components = vec![serde_json::json!({
        "uid": "net.minecraft",
        "version": mc_version,
        "important": true,
    })];
    if loader != LoaderKind::Vanilla {
        // Гард выше гарантирует точную версию — здесь только забираем её.
        let lv = loader_version.expect("guarded exact version");
        components.push(serde_json::json!({
            "uid": prism_component_uid(loader),
            "version": lv,
        }));
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("instance.cfg"),
        format!(
            "ConfigVersion=1.3\nInstanceType=OneSix\nManagedPack=false\nname={display_name}\n"
        ),
    )?;
    std::fs::write(
        dir.join("mmc-pack.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "formatVersion": 1,
            "components": components,
        }))
        .expect("json"),
    )?;
    set_prism_icon(data_dir, &dir, &slug, icon_path);

    Ok(GameTarget {
        launcher: LauncherKind::Prism,
        name: display_name.to_string(),
        path: dir,
        minecraft_version: mc_version.to_string(),
        loader,
        loader_version: loader_version.map(|s| s.to_string()),
    })
}

fn prism_component_uid(loader: LoaderKind) -> &'static str {
    match loader {
        LoaderKind::Fabric => "net.fabricmc.fabric-loader",
        LoaderKind::Forge => "net.minecraftforge",
        LoaderKind::Quilt => "org.quiltmc.quilt-loader",
        LoaderKind::NeoForge => "net.neoforged",
        LoaderKind::Vanilla => "net.minecraft",
    }
}

/// Иконка инстанса: копия в `<data>/icons/<slug>.png` + `iconKey` в cfg.
/// Вход любой (WebP с сайта, JPEG из архива) — конвертер доводит до PNG.
/// Best-effort: любая неудача молча пропускается, без предупреждений.
fn set_prism_icon(data_dir: &Path, instance_dir: &Path, slug: &str, icon_path: Option<&Path>) {
    let src = match icon_path {
        Some(p) => p,
        None => return,
    };
    let png = match crate::services::icon::ensure_png(src) {
        Some(p) => p,
        None => return,
    };
    let icons = data_dir.join("icons");
    if std::fs::create_dir_all(&icons).is_err() {
        return;
    }
    if std::fs::copy(&png, icons.join(format!("{slug}.png"))).is_err() {
        return;
    }
    let cfg_path = instance_dir.join("instance.cfg");
    if let Ok(mut cfg) = std::fs::read_to_string(&cfg_path) {
        if !cfg.contains("iconKey=") {
            cfg.push_str(&format!("iconKey={slug}\n"));
            let _ = std::fs::write(&cfg_path, cfg);
        }
    }
}

/// Свободный slug папки: занят → `<slug>_1`, `<slug>_2`...
fn unique_slug(instances: &Path, base: &str) -> String {
    if !instances.join(base).exists() {
        return base.to_string();
    }
    let mut n = 1u32;
    loop {
        let candidate = format!("{base}_{n}");
        if !instances.join(&candidate).exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Аргументы запуска Prism (чистая функция — тестируется без сайд-эффектов).
/// Проверено по документации CLI: `--launch <id> [--world <мир>]`.
pub fn prism_launch_args(instance_id: &str, world: Option<&str>) -> Vec<String> {
    let mut args = vec!["--launch".to_string(), instance_id.to_string()];
    if let Some(w) = world.filter(|w| !w.is_empty()) {
        args.push("--world".to_string());
        args.push(w.to_string());
    }
    args
}

/// Запуск игры через Prism. Возвращает команду для «Подробнее».
pub fn launch_prism(exe: &Path, instance_id: &str, world: Option<&str>) -> Result<String, InstallError> {
    let args = prism_launch_args(instance_id, world);
    std::process::Command::new(exe)
        .args(&args)
        .spawn()
        .map_err(|e| InstallError::LaunchFailed(format!("{} {}: {e}", exe.display(), args.join(" "))))?;
    Ok(format!("{} {}", exe.display(), args.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-prism-{tag}"))
    }
    use std::path::PathBuf;

    #[test]
    fn creates_vanilla_instance_with_recipe_files() {
        let root = temp_root("vanilla");
        let target = create_prism_instance(&root, "DeepMine", "1.21.11", LoaderKind::Vanilla, None, None)
            .expect("create");
        assert_eq!(target.name, "DeepMine");
        assert!(target.path.join("instance.cfg").is_file());
        let cfg = std::fs::read_to_string(target.path.join("instance.cfg")).expect("cfg");
        assert!(cfg.contains("ManagedPack=false"));
        assert!(cfg.contains("InstanceType=OneSix"));
        let mmc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(target.path.join("mmc-pack.json")).expect("mmc"))
                .expect("json");
        let uids: Vec<&str> = mmc["components"]
            .as_array()
            .expect("components")
            .iter()
            .map(|c| c["uid"].as_str().expect("uid"))
            .collect();
        assert_eq!(uids, ["net.minecraft"]);
        // Инстанс виден детекту.
        let found = crate::services::detect::enumerate_prism_targets(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].minecraft_version, "1.21.11");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn modded_instance_gets_loader_component_and_unique_slug() {
        let root = temp_root("modded");
        std::fs::create_dir_all(root.join("instances").join("TowerofDoom")).expect("taken");
        let target = create_prism_instance(
            &root,
            "Tower of Doom",
            "1.18.2",
            LoaderKind::Forge,
            Some("40.3.0"),
            None,
        )
        .expect("create");
        assert!(target.path.ends_with("TowerofDoom_1"));
        let mmc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(target.path.join("mmc-pack.json")).expect("mmc"))
                .expect("json");
        let forge = mmc["components"]
            .as_array()
            .expect("components")
            .iter()
            .find(|c| c["uid"] == "net.minecraftforge")
            .expect("forge component");
        assert_eq!(forge["version"], "40.3.0");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn launch_args_include_world() {
        assert_eq!(
            prism_launch_args("DeepMine", Some("DeepMine")),
            ["--launch", "DeepMine", "--world", "DeepMine"]
        );
        assert_eq!(prism_launch_args("DeepMine", None), ["--launch", "DeepMine"]);
    }

    fn fake_png(dir: &Path, name: &str) -> PathBuf {
        // Настоящий PNG 1x1 через энкодер (ручные байты могут не декодироваться).
        let img = image::RgbImage::from_pixel(1, 1, image::Rgb([70, 130, 180]));
        let p = dir.join(name);
        img.save_with_format(&p, image::ImageFormat::Png).expect("png");
        p
    }

    #[test]
    fn png_icon_is_linked_via_icon_key() {
        let root = temp_root("icon");
        let icon = fake_png(&root, "site-icon.png");
        let target =
            create_prism_instance(&root, "DeepMine", "1.21.11", LoaderKind::Vanilla, None, Some(&icon))
                .expect("create");
        assert!(root.join("icons").join("DeepMine.png").is_file());
        let cfg = std::fs::read_to_string(target.path.join("instance.cfg")).expect("cfg");
        assert!(cfg.contains("iconKey=DeepMine"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn non_png_icon_is_skipped_silently() {
        let root = temp_root("icon-skip");
        let icon = fake_png(&root, "site-icon.webp");
        let target =
            create_prism_instance(&root, "DeepMine", "1.21.11", LoaderKind::Vanilla, None, Some(&icon))
                .expect("create");
        assert!(!root.join("icons").join("DeepMine.png").exists());
        let cfg = std::fs::read_to_string(target.path.join("instance.cfg")).expect("cfg");
        assert!(!cfg.contains("iconKey="));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn jpeg_icon_converts_and_links() {
        let root = temp_root("icon-jpg");
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([30, 120, 200]));
        let icon = root.join("site-icon.jpg");
        img.save_with_format(&icon, image::ImageFormat::Jpeg).expect("jpg");
        let target =
            create_prism_instance(&root, "DeepMine", "1.21.11", LoaderKind::Vanilla, None, Some(&icon))
                .expect("create");
        let linked = root.join("icons").join("DeepMine.png");
        assert!(linked.is_file());
        // Лежит настоящий PNG, а не переименованный JPEG.
        let back = image::open(&linked).expect("open");
        assert_eq!(back.width(), 2);
        let cfg = std::fs::read_to_string(target.path.join("instance.cfg")).expect("cfg");
        assert!(cfg.contains("iconKey=DeepMine"));
        std::fs::remove_dir_all(&root).ok();
    }
}
