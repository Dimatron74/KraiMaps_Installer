//! Native-авторизация установщика (закрытый OAuth-провайдер сайта, RFC 8252).
//!
//! Поток:
//! 1. `oauth_start` — генерим PKCE (verifier/challenge S256) + state, отдаём
//!    URL consent-страницы. Фронт открывает его в **системном браузере**.
//! 2. Пользователь входит и жмёт «Разрешить» → сайт уходит на
//!    `kraimaps://oauth/callback?code=..&state=..` → ОС будит установщик.
//! 3. `oauth_exchange` — проверяем state, меняем code+verifier на пару
//!    access (15 мин) + refresh (30 дней). Токены — только в OS keyring.
//! 4. `access_token` — отдаёт живой access, молча обновляя по refresh
//!    (конкурентные refresh сериализованы + дедуп через перечитывание).
//! 5. `refresh_now` — принудительная ротация для 401-retry.
//! 6. `logout` — `POST /auth/oauth/revoke` + чистка keyring.
//!
//! В логах токенов нет. В JS токены не попадают никогда.

use crate::domain::InstallError;
use crate::services::site;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const SERVICE: &str = "com.kraimaps.installer";
const ACCOUNT: &str = "native-tokens";
const CLIENT_ID: &str = "kraimaps-installer";
const REDIRECT_URI: &str = "kraimaps://oauth/callback";
const SCOPES: &str = "profile:read maps:install library:read";
/// Заранее обновляем access, если до конца жизни осталось меньше этого.
const REFRESH_MARGIN_SECS: u64 = 60;

#[derive(Debug, Clone)]
struct Pending {
    verifier: String,
    state: String,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTokens {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[allow(dead_code)]
    token_type: Option<String>,
    expires_in: Option<i64>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// base64url без паддинга (для PKCE challenge). Отдельной зависимости нет спецом.
fn b64url_nopad(data: &[u8]) -> String {
    const ALPH: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((data.len() * 4).div_ceil(3));
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPH[((n >> 18) & 63) as usize] as char);
        out.push(ALPH[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPH[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPH[(n & 63) as usize] as char);
        }
    }
    out
}

fn pkce_pair() -> (String, String) {
    // verifier: 128 hex-символов (допустимо 43–128, hex — подмножество unreserved).
    let verifier = format!(
        "{}{}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let digest = {
        use sha2::Digest;
        sha2::Sha256::digest(verifier.as_bytes())
    };
    (verifier, b64url_nopad(&digest))
}

fn entry() -> Result<keyring::Entry, InstallError> {
    keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|e| InstallError::LaunchFailed(format!("keyring: {e}")))
}

fn load_tokens() -> Option<StoredTokens> {
    let raw = entry().ok()?.get_password().ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_tokens(t: &StoredTokens) -> Result<(), InstallError> {
    let raw =
        serde_json::to_string(t).map_err(|e| InstallError::LaunchFailed(format!("tokens: {e}")))?;
    entry()?.set_password(&raw).map_err(|e| {
        // Провал записи — критическая ошибка (I-4.1): молчать нельзя, иначе
        // следующий старт прочтёт старый refresh как reuse. Подсказка про
        // Secret Service — только текст для «Подробнее», секретов нет.
        #[cfg(target_os = "linux")]
        let hint = " (нужна служба Secret Service: gnome-keyring или KWallet)";
        #[cfg(not(target_os = "linux"))]
        let hint = "";
        InstallError::LaunchFailed(format!("keyring save: {e}{hint}"))
    })
}

/// Шаг 1: URL consent-страницы для системного браузера.
pub fn oauth_start_url() -> Result<String, InstallError> {
    let (verifier, challenge) = pkce_pair();
    let state = uuid::Uuid::new_v4().simple().to_string();
    *PENDING
        .lock()
        .map_err(|e| InstallError::LaunchFailed(format!("pending: {e}")))? =
        Some(Pending { verifier, state: state.clone() });
    // Скопы — через точку с запятой не едут: кодируем пробелы как %20 вручную,
    // чтобы не тянуть urlencoding-зависимость.
    let scope = SCOPES.replace(' ', "%20");
    Ok(format!(
        "{}/oauth/authorize?client_id={CLIENT_ID}&redirect_uri={REDIRECT_URI}&code_challenge={challenge}&code_challenge_method=S256&state={state}&scope={scope}",
        site::site_base()
    ))
}

/// Шаг 3: обмен code+verifier на native-пару (проверяем state из callback).
///
/// Идемпотентность: PENDING живёт только до первого обмена. Повторный или
/// опоздавший callback (двойной клик «Разрешить», дубль deep-link) при живой
/// сохранённой сессии — успех без повторного обмена, а не разлогинивание.
/// Без живой сессии — честная ошибка «вход не начинался».
pub async fn oauth_exchange(code: &str, state: &str) -> Result<(), InstallError> {
    let pending = PENDING
        .lock()
        .map_err(|e| InstallError::LaunchFailed(format!("pending: {e}")))?
        .clone();
    let pending = match pending {
        Some(p) => p,
        None => {
            // access_token() молча освежает протухший access по refresh;
            // мёртвая сессия при этом чистится — и это правильно.
            if access_token().await.is_ok() {
                return Ok(());
            }
            return Err(InstallError::AuthRequired("вход не начинался".to_string()));
        }
    };
    if pending.state != state {
        return Err(InstallError::AuthRequired(
            "ответ сайта не совпал с запросом (state)".to_string(),
        ));
    }
    let client = site::http_client()?;
    let resp = client
        .post(format!("{}/auth/oauth/token", site::api_base()))
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": pending.verifier,
        }))
        .send()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("token: {e}")))?;
    if !resp.status().is_success() {
        return Err(site::err_for_status(resp, "обмен кода").await);
    }
    let tok: TokenResponse = resp
        .json()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("token parse: {e}")))?;
    let ttl = tok.expires_in.unwrap_or(900).max(60) as u64;
    save_tokens(&StoredTokens {
        access_token: tok.access_token,
        refresh_token: tok.refresh_token,
        expires_at: now_secs().saturating_add(ttl),
    })?;
    *PENDING
        .lock()
        .map_err(|e| InstallError::LaunchFailed(format!("pending: {e}")))? = None;
    Ok(())
}

/// Сериализатор конкурентных refresh: параллельные команды делят одну
/// ротацию (аналог `sessionRefreshPromise` на сайте, I-2). Без него N
/// параллельных команд устраивали бы N конкурентных ротаций.
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Нужен ли refresh: до конца жизни access осталось меньше маржи.
fn needs_refresh(expires_at: u64, now: u64) -> bool {
    expires_at.saturating_sub(REFRESH_MARGIN_SECS) <= now
}

async fn refresh_locked(refresh_token: &str) -> Result<StoredTokens, InstallError> {
    let _guard = REFRESH_LOCK.lock().await;
    // Дедуп: пока ждали замок, соседняя команда уже могла ротировать —
    // тогда на диске уже новый refresh и повторный HTTP не нужен.
    let effective = match load_tokens() {
        Some(stored) if stored.refresh_token != refresh_token => {
            if !needs_refresh(stored.expires_at, now_secs()) {
                return Ok(stored);
            }
            stored.refresh_token.clone()
        }
        _ => refresh_token.to_string(),
    };
    refresh_inner(&effective).await
}

async fn refresh_inner(refresh_token: &str) -> Result<StoredTokens, InstallError> {
    let client = site::http_client()?;
    let resp = client
        .post(format!("{}/auth/oauth/token", site::api_base()))
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }))
        .send()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("refresh: {e}")))?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        // Refresh мёртв — сессию не спасти, чистим и просим войти заново.
        let _ = entry()?.delete_credential();
        return Err(InstallError::AuthRequired("сессия истекла".to_string()));
    }
    if !resp.status().is_success() {
        return Err(site::err_for_status(resp, "обновление сессии").await);
    }
    let tok: TokenResponse = resp
        .json()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("refresh parse: {e}")))?;
    let ttl = tok.expires_in.unwrap_or(900).max(60) as u64;
    let stored = StoredTokens {
        access_token: tok.access_token,
        refresh_token: tok.refresh_token,
        expires_at: now_secs().saturating_add(ttl),
    };
    save_tokens(&stored)?;
    Ok(stored)
}

/// Живой access-токен (молча обновляет по refresh при нужде).
pub async fn access_token() -> Result<String, InstallError> {
    let stored =
        load_tokens().ok_or_else(|| InstallError::AuthRequired("нет сессии".to_string()))?;
    if needs_refresh(stored.expires_at, now_secs()) {
        return Ok(refresh_locked(&stored.refresh_token).await?.access_token);
    }
    Ok(stored.access_token)
}

/// Принудительное обновление access вне зависимости от срока жизни —
/// для 401-retry (I-2): сервер отверг даже свежий access, пробуем ротацию
/// и повторяем запрос один раз. Исходы как у `access_token`: `AuthRequired`
/// только при мёртвом refresh (401), сеть/5xx/429 — транзиент, не гость.
pub async fn refresh_now() -> Result<String, InstallError> {
    let stored =
        load_tokens().ok_or_else(|| InstallError::AuthRequired("нет сессии".to_string()))?;
    Ok(refresh_locked(&stored.refresh_token).await?.access_token)
}

/// Состояние хранилища сессии для UI (I-4.2). `persistent=false` означает,
/// что запись недоступна (на Linux обычно нет Secret Service) — сессия не
/// переживёт перезапуск. Фронт показывает предупреждение, а не тихий гость.
/// Секретов в ответе нет.
pub fn store_status() -> serde_json::Value {
    match entry() {
        Err(e) => serde_json::json!({ "persistent": false, "detail": format!("keyring: {e}") }),
        Ok(en) => match en.get_password() {
            Ok(_) => serde_json::json!({ "persistent": true, "detail": "ok" }),
            Err(keyring::Error::NoEntry) => {
                serde_json::json!({ "persistent": true, "detail": "empty" })
            }
            Err(e) => {
                serde_json::json!({ "persistent": false, "detail": format!("keyring: {e}") })
            }
        },
    }
}

/// Выход: гасим native-сессию на сайте, чистим keyring в любом случае.
pub async fn logout() -> Result<(), InstallError> {
    let stored = load_tokens();
    if let Some(t) = stored {
        let client = site::http_client()?;
        // Best effort: даже при сетевой ошибке keyring чистим ниже.
        let _ = client
            .post(format!("{}/auth/oauth/revoke", site::api_base()))
            .bearer_auth(&t.access_token)
            .send()
            .await;
    }
    match entry()?.delete_credential() {
        Ok(()) => Ok(()),
        // Нет записи — уже вышли.
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(InstallError::LaunchFailed(format!("keyring clear: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_access_needs_refresh() {
        // Протухший access — освежать; свежий — отдавать как есть (I-1).
        assert!(needs_refresh(1000, 2000));
        assert!(needs_refresh(1960, 2000)); // внутри 60с-маржи — тоже освежать
        assert!(!needs_refresh(2061, 2000));
    }

    #[test]
    fn b64url_known_vector() {
        // "foo" → "Zm9v" (RFC 4648, без паддинга).
        assert_eq!(b64url_nopad(b"foo"), "Zm9v");
        assert_eq!(b64url_nopad(b"f"), "Zg");
    }

    #[test]
    fn pkce_verifier_length_ok() {
        let (v, c) = pkce_pair();
        assert!((43..=128).contains(&v.len()));
        assert!(!c.contains(['+', '/', '=']));
    }
}
