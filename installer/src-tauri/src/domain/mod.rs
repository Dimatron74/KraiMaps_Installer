//! Доменные типы установщика: ошибки с кодами `E_*`, виды лаунчеров и
//! загрузчиков, модель нормализованного пака. Источник истины по поведению —
//! `docs/установщик.md` §§6.3–6.6 и дизайн-док от 19.09.2026.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Все ошибки, которые может показать UI. Каждая имеет стабильный код `E_*`
/// для техдеталей («Подробнее») и логов. Секретов в сообщениях нет.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("не удалось открыть zip-архив")]
    ZipOpen(#[source] std::io::Error),
    #[error("zip-архив повреждён")]
    ZipCorrupt(String),
    #[error("в архиве нет мира (файл level.dat не найден)")]
    NoLevelDat,
    #[error("в архиве несколько разных миров, непонятно какой ставить")]
    MultipleWorlds(Vec<String>),
    #[error("архив содержит небезопасные пути")]
    ZipSlip(String),
    #[error("архив превышает лимиты безопасности")]
    LimitExceeded(String),
    #[error("контрольная сумма файла не сошлась")]
    // Этап 2 (скачивание с сайта): сверка sha256 с манифестом.
    #[allow(dead_code)]
    HashMismatch { expected: String, actual: String },
    #[error("не найден ни один поддерживаемый лаунчер")]
    NoLauncher,
    #[error("не найдена папка данных лаунчера")]
    NoDataDir { launcher: String },
    #[error("неизвестный формат базы Modrinth App")]
    ModrinthSchema(String),
    #[error("неизвестный загрузчик модов, угадывать нельзя")]
    UnknownLoader(String),
    #[error("нет подходящей Java для подготовки версии")]
    NoJava(String),
    #[error("не удалось подготовить версию игры")]
    ProvisionFailed(String),
    #[error("ошибка запуска игры")]
    LaunchFailed(String),
    /// Сайт: нужна авторизация (401). UI ведёт на экран входа.
    #[error("нужен вход в аккаунт")]
    AuthRequired(String),
    /// Сайт: доступ запрещён (403 — платная карта или нет прав).
    #[error("установка недоступна")]
    AccessDenied(String),
    /// Сайт: не найдено (404).
    #[error("не найдено на сайте")]
    NotFound(String),
    /// Сайт: прочая ошибка API (409 нет файла, 429 лимит, 5xx).
    #[error("сайт вернул ошибку")]
    SiteApi(String),
    /// Сайт: временно недоступен (503 — техработы/maintenance).
    /// Отдельный вариант, чтобы UI говорил «попробуй позже», а не пугал.
    #[error("сайт временно недоступен")]
    ServiceUnavailable(String),
    /// Обновления: нет соединения с сервером обновлений.
    /// Fail-open: автопроверка молчит, ручная показывает тост. Блокировка
    /// работы — только по явному сигналу сервера (`min_supported_version`),
    /// никогда по факту «сеть упала».
    #[error("нет соединения с сервером обновлений")]
    UpdateOffline(String),
    /// Обновления: проверка не удалась (HTTP-ошибка, битый JSON).
    #[error("не удалось проверить обновления")]
    UpdateCheckFailed(String),
    /// Обновления: подпись сборки не сошлась. Критично: установка
    /// запрещена самим плагином, UI показывает стоп-экран с «Подробнее».
    #[error("подпись обновления не сошлась")]
    UpdateSignature(String),
    /// Обновления: скачивание или установка сборки не удалась.
    #[error("не удалось установить обновление")]
    UpdateInstallFailed(String),
    #[error("ошибка файловой системы")]
    Io(#[from] std::io::Error),
    #[error("ошибка базы данных")]
    Db(#[from] rusqlite::Error),
}

impl InstallError {
    /// Сессия доказано мертва (можно показывать экран входа).
    /// Единственный фатальный вариант — `AuthRequired`: refresh упал с 401.
    /// Сеть, 5xx, 429 (`SiteApi`/`ServiceUnavailable`/`LaunchFailed`) и чужие
    /// 401/403/404 — транзиент: гостем не объявлять, статус сохранять (I-3).
    pub fn is_session_dead(&self) -> bool {
        matches!(self, Self::AuthRequired(_))
    }

    /// Стабильный код для UI («Подробнее»), логов и поддержки.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ZipOpen(_) => "E_ZIP_OPEN",
            Self::ZipCorrupt(_) => "E_ZIP_CORRUPT",
            Self::NoLevelDat => "E_NO_LEVEL_DAT",
            Self::MultipleWorlds(_) => "E_MULTI_WORLD",
            Self::ZipSlip(_) => "E_ZIPSLIP",
            Self::LimitExceeded(_) => "E_LIMIT",
            Self::HashMismatch { .. } => "E_HASH_MISMATCH",
            Self::NoLauncher => "E_NO_LAUNCHER",
            Self::NoDataDir { .. } => "E_NO_DATA_DIR",
            Self::ModrinthSchema(_) => "E_MODRINTH_SCHEMA",
            Self::UnknownLoader(_) => "E_UNKNOWN_LOADER",
            Self::NoJava(_) => "E_NO_JAVA",
            Self::ProvisionFailed(_) => "E_PROVISION_FAILED",
            Self::LaunchFailed(_) => "E_LAUNCH_FAILED",
            Self::AuthRequired(_) => "E_UNAUTHORIZED",
            Self::AccessDenied(_) => "E_ACCESS_DENIED",
            Self::NotFound(_) => "E_NOT_FOUND",
            Self::SiteApi(_) => "E_IO",
            Self::ServiceUnavailable(_) => "E_UNAVAILABLE",
            Self::UpdateOffline(_) => "E_UPDATE_OFFLINE",
            Self::UpdateCheckFailed(_) => "E_UPDATE_CHECK",
            Self::UpdateSignature(_) => "E_UPDATE_SIGNATURE",
            Self::UpdateInstallFailed(_) => "E_UPDATE_INSTALL",
            Self::Io(_) => "E_IO",
            Self::Db(_) => "E_DB",
        }
    }

    /// Технические детали для сворачиваемого блока «Подробнее». Без секретов.
    pub fn details(&self) -> String {
        match self {
            Self::ZipOpen(e) => format!("open failed: {e}"),
            Self::ZipCorrupt(what) => format!("corrupt: {what}"),
            Self::MultipleWorlds(roots) => format!("worlds: {}", roots.join(", ")),
            Self::ZipSlip(entry) => format!("unsafe entry: {entry}"),
            Self::LimitExceeded(what) => format!("limit: {what}"),
            Self::HashMismatch { expected, actual } => {
                format!("expected sha256 {expected}, got {actual}")
            }
            Self::NoDataDir { launcher } => format!("launcher: {launcher}"),
            Self::ModrinthSchema(what) => format!("schema: {what}"),
            Self::UnknownLoader(hint) => format!("hint: {hint}"),
            Self::NoJava(hint) => format!("hint: {hint}"),
            Self::ProvisionFailed(hint) => format!("hint: {hint}"),
            Self::LaunchFailed(cmd) => format!("command: {cmd}"),
            Self::AuthRequired(ctx) => format!("auth required: {ctx}"),
            Self::AccessDenied(ctx) => format!("access denied: {ctx}"),
            Self::NotFound(ctx) => format!("not found: {ctx}"),
            Self::SiteApi(ctx) => format!("site api: {ctx}"),
            Self::ServiceUnavailable(ctx) => format!("service unavailable: {ctx}"),
            Self::UpdateOffline(ctx) => format!("updates offline: {ctx}"),
            Self::UpdateCheckFailed(ctx) => format!("updates check: {ctx}"),
            Self::UpdateSignature(ctx) => format!("updates signature: {ctx}"),
            Self::UpdateInstallFailed(ctx) => format!("updates install: {ctx}"),
            Self::Io(e) => format!("io: {e}"),
            Self::Db(e) => format!("db: {e}"),
            _ => self.code().to_string(),
        }
    }
}

/// Поддерживаемые лаунчеры. Prism-семейство (Prism, Pinecone — бывший ElyPrism,
/// PolyMC, MultiMC: общий формат `instances/*/instance.cfg + mmc-pack.json`
/// и CLI `--launch <id>`) + Modrinth (+ ручная папка для ваниллы),
/// плюс TLauncher (общий game dir без инстансов, версии в `versions/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LauncherKind {
    Prism,
    Pinecone,
    Polymc,
    Multimc,
    Modrinth,
    Manual,
    Tlauncher,
}

/// Загрузчик модов. `Vanilla` — мир без модов (живёт в любом инстансе).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    Vanilla,
    Fabric,
    Forge,
    Quilt,
    NeoForge,
}

/// Игровая цель: инстанс/профиль, куда можно ставить карту.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameTarget {
    pub launcher: LauncherKind,
    /// Человекочитаемое имя (инстанс Prism-семейства, профиль Modrinth, имя папки).
    pub name: String,
    /// Корень цели: у Prism-семейства — папка инстанса, у Modrinth — папка профиля.
    pub path: PathBuf,
    pub minecraft_version: String,
    pub loader: LoaderKind,
    pub loader_version: Option<String>,
}

/// Найденный лаунчер: исполняемый файл + корень данных + цели в нём.
/// Одного вида может быть несколько (стандартный + портативный) — каждый
/// со своим корнем данных. `portable` — данные рядом с exe, а не в APPDATA.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherInfo {
    pub kind: LauncherKind,
    pub display_name: String,
    pub exe_path: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub portable: bool,
    pub targets: Vec<GameTarget>,
}

/// Результат анализа zip: нормализованная модель пака (§14 ТЗ).
/// Деплой стримится из исходного архива напрямую, без промежуточного `.mrpack`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackAnalysis {
    pub file_name: String,
    pub file_size_bytes: u64,
    pub sha256: String,
    /// Имя мира = имя родителя `level.dat`.
    pub world_name: String,
    /// Версия мира из `level.dat → Version.Name` (может отсутствовать).
    pub world_version: Option<String>,
    pub is_modded: bool,
    pub loader: LoaderKind,
    /// Подсказка версии загрузчика из jar-манифестов (`[40,)` и т.п.).
    pub loader_version_hint: Option<String>,
    /// `mod-id → версия` для бейджа «N модов».
    pub mods: Vec<ModInfo>,
    /// Верхние слои пака, которые будут разложены (config, resourcepacks...).
    pub layers: Vec<String>,
    pub total_uncompressed_bytes: u64,
    pub entry_count: usize,
    /// Путь внутрь архива к корню мира (родитель `level.dat`), с `/` на конце.
    pub world_prefix: String,
    /// Корень контента: общая папка-обёртка мира и слоёв (`"Tower of Doom/"`)
    /// или `""` для плоских архивов. Слои (`mods/` и т.д.) считаются от него.
    pub content_prefix: String,
    /// В корне контента лежит `modrinth.index.json`: ведущий `overrides/`
    /// это псевдоним корня, а не папка для копирования.
    pub overrides_alias: bool,
    /// Loose-ресурспаки рядом с миром (папки и zip вне `world_prefix`
    /// и вне `resourcepacks/`). Ставятся в `resourcepacks/` цели;
    /// выбор пользователя (галочки) фронт присылает в плане деплоя.
    #[serde(default)]
    pub resource_packs: Vec<ResourcePackInfo>,
}

/// Один мод из `mods/*.jar` в корне архива.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModInfo {
    pub file_name: String,
    pub mod_id: String,
    pub version: Option<String>,
    pub loader: LoaderKind,
    pub size_bytes: u64,
}

/// Вид найденного ресурспака в архиве карты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourcePackKind {
    /// Распакованная папка: `<Имя>/pack.mcmeta` + `<Имя>/assets/…`.
    Folder,
    /// Zip-файл в архиве, в корне которого `pack.mcmeta` + `assets/…`.
    Zip,
}

/// Ресурспак, найденный рядом с миром (вне `world_prefix`).
/// Маркер — `pack.mcmeta` (+ `assets/` отличает от датапаков, у них `data/`).
/// Встроенные (`<мир>/resources.zip`, `<мир>/datapacks/*/pack.mcmeta`) сюда
/// не попадают — они едут вместе с миром и игрой подхватываются сами.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePackInfo {
    /// Человекочитаемое имя: папка — basename, zip — имя файла с `.zip`.
    pub name: String,
    /// Источник в архиве: папка — префикс с `/` на конце, zip — путь записи.
    pub source: String,
    pub kind: ResourcePackKind,
    /// `pack.description` из `pack.mcmeta` (best-effort, может отсутствовать:
    /// новый формат `min_format`/`max_format` вместо `pack_format` — тоже ок).
    pub description: Option<String>,
}

/// Выбор пользователя для деплоя: только включённые паки едут в план.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePackSelection {
    pub source: String,
    pub name: String,
    pub kind: ResourcePackKind,
}

/// Имя ресурспака в `resourcepacks/` с учётом конфликта. Политика как у миров:
/// замена запрещена, занятое имя даёт суффикс `_v2`, `_v3`... Папки получают
/// суффикс целиком (`Pack_v2`), zip — перед расширением (`Pack_v2.zip`).
/// `exists` проверяет наличие файла ИЛИ папки с таким именем в цели.
pub fn resolve_resourcepack_name(base: &str, kind: ResourcePackKind, exists: &dyn Fn(&str) -> bool) -> String {
    if !exists(base) {
        return base.to_string();
    }
    let (stem, ext) = match kind {
        ResourcePackKind::Folder => (base.to_string(), String::new()),
        ResourcePackKind::Zip => match base.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), format!(".{e}")),
            None => (base.to_string(), String::new()),
        },
    };
    let mut n = 2u32;
    loop {
        let candidate = format!("{stem}_v{n}{ext}");
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Имя папки инстанса (Prism-семейство) / профиля Modrinth из названия карты.
/// Буквы (включая кириллицу — профили вроде «Хардкор 1.0» существуют
/// вживую) / цифры / `-` / `_` как есть, пробелы и точки выкидываем
/// (`"Tower of Doom"` → `"TowerofDoom"`). Пустой результат → `"Map"`,
/// чтобы никогда не создавать безымянные папки.
pub fn slugify(name: &str) -> String {
    let slug: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if slug.is_empty() {
        "Map".to_string()
    } else {
        slug
    }
}

/// Отображаемое имя инстанса/профиля (пишется в `instance.cfg name`
/// и строку `instances.name`). Система разрешённого: буквы (включая
/// кириллицу), цифры, пробелы и `-. _()[]!',+`; всё остальное — включая
/// запрещённые в путях Windows `\/:*?"<>|` и управляющие символы —
/// фильтруется. Пробелы схлопываем, длина — до 80 символов.
/// Пустое после чистки → `"Map"`.
pub fn sanitize_display_name(raw: &str) -> String {
    let kept: String = raw
        .chars()
        .filter(|c| {
            // Пробельные (табы/переносы) не выкидываем, а оставляем —
            // ниже схлопнутся в обычные пробелы; иначе слова склеятся.
            c.is_alphanumeric()
                || c.is_whitespace()
                || matches!(
                    c,
                    '-' | '_' | '.' | '(' | ')' | '[' | ']' | '!' | '\'' | '+' | ','
                )
        })
        .collect();
    let collapsed = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed: String = collapsed.chars().take(80).collect();
    let trimmed = trimmed.trim().to_string();
    if trimmed.is_empty() {
        "Map".to_string()
    } else {
        trimmed
    }
}

/// Имя мира в `saves/` с учётом конфликта. Замена запрещена всегда: занятое имя
/// даёт суффикс `_v2`, `_v3`... (`exists` проверяет наличие папки в цели).
pub fn resolve_world_name(base: &str, exists: &dyn Fn(&str) -> bool) -> String {
    if !exists(base) {
        return base.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}_v{n}");
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_removes_spaces_and_keeps_case() {
        assert_eq!(slugify("Tower of Doom"), "TowerofDoom");
        assert_eq!(slugify("DeepMine"), "DeepMine");
        assert_eq!(slugify("My  Cool-Map_2"), "MyCool-Map_2");
        assert_eq!(slugify("Хардкор 1.0"), "Хардкор10");
    }

    #[test]
    fn slugify_falls_back_when_nothing_left() {
        assert_eq!(slugify("!!!"), "Map");
        assert_eq!(slugify("   "), "Map");
    }

    #[test]
    fn only_auth_required_means_dead_session() {
        // FATAL: сессия доказано мертва — только AuthRequired.
        assert!(InstallError::AuthRequired("x".to_string()).is_session_dead());
        // TRANSIENT: всё остальное гостем не объявляет (I-3).
        assert!(!InstallError::SiteApi("x".to_string()).is_session_dead());
        assert!(!InstallError::ServiceUnavailable("x".to_string()).is_session_dead());
        assert!(!InstallError::LaunchFailed("net".to_string()).is_session_dead());
        assert!(!InstallError::AccessDenied("x".to_string()).is_session_dead());
        assert!(!InstallError::NotFound("x".to_string()).is_session_dead());
    }

    #[test]
    fn error_codes_use_e_prefix() {
        assert_eq!(InstallError::NoLevelDat.code(), "E_NO_LEVEL_DAT");
        assert_eq!(
            InstallError::MultipleWorlds(vec![]).code(),
            "E_MULTI_WORLD"
        );
        assert_eq!(InstallError::ZipSlip(String::new()).code(), "E_ZIPSLIP");
    }

    #[test]
    fn world_conflict_appends_version_suffix() {
        let existing = ["DeepMine".to_string()];
        let exists = |n: &str| existing.iter().any(|e| e == n);
        assert_eq!(resolve_world_name("DeepMine", &exists), "DeepMine_v2");
        assert_eq!(resolve_world_name("Fresh", &exists), "Fresh");
    }

    #[test]
    fn world_conflict_increments_suffix() {
        let existing = ["DeepMine".to_string(), "DeepMine_v2".to_string()];
        let exists = |n: &str| existing.iter().any(|e| e == n);
        assert_eq!(resolve_world_name("DeepMine", &exists), "DeepMine_v3");
    }

    #[test]
    fn resourcepack_conflict_appends_version_suffix() {
        let existing = ["RP".to_string()];
        let exists = |n: &str| existing.iter().any(|e| e == n);
        assert_eq!(
            resolve_resourcepack_name("RP", ResourcePackKind::Folder, &exists),
            "RP_v2"
        );
        assert_eq!(
            resolve_resourcepack_name("Fresh", ResourcePackKind::Folder, &exists),
            "Fresh"
        );
    }

    #[test]
    fn resourcepack_zip_conflict_suffixes_before_extension() {
        let existing = ["Cute.zip".to_string(), "Cute_v2.zip".to_string()];
        let exists = |n: &str| existing.iter().any(|e| e == n);
        assert_eq!(
            resolve_resourcepack_name("Cute.zip", ResourcePackKind::Zip, &exists),
            "Cute_v3.zip"
        );
    }

    #[test]
    fn display_name_filters_forbidden_chars() {
        assert_eq!(
            sanitize_display_name("Tower of Doom: Episode I/II *FINAL*?"),
            "Tower of Doom Episode III FINAL"
        );
        assert_eq!(
            sanitize_display_name("  Хардкор\\n1.0\t(сезон 2)  "),
            "Хардкорn1.0 (сезон 2)"
        );
        assert_eq!(sanitize_display_name("???"), "Map");
        assert_eq!(sanitize_display_name(""), "Map");
    }

    #[test]
    fn tlauncher_kind_uses_lowercase_wire_name() {
        assert_eq!(
            serde_json::to_string(&LauncherKind::Tlauncher).expect("json"),
            "\"tlauncher\""
        );
    }

    #[test]
    fn mmc_family_kinds_use_lowercase_wire_names() {
        assert_eq!(
            serde_json::to_string(&LauncherKind::Pinecone).expect("json"),
            "\"pinecone\""
        );
        assert_eq!(
            serde_json::to_string(&LauncherKind::Polymc).expect("json"),
            "\"polymc\""
        );
        assert_eq!(
            serde_json::to_string(&LauncherKind::Multimc).expect("json"),
            "\"multimc\""
        );
    }

    #[test]
    fn provision_error_codes_use_e_prefix() {
        assert_eq!(
            InstallError::NoJava("нет Java 17+".to_string()).code(),
            "E_NO_JAVA"
        );
        assert_eq!(
            InstallError::ProvisionFailed("installer failed".to_string()).code(),
            "E_PROVISION_FAILED"
        );
    }
}
