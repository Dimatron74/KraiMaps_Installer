/**
 * Конфиг связи «установщик → сайт».
 * Сайт указывает — установщик подстраивается.
 *
 * Базовый URL API можно переопределить через .env (VITE_KRAIMAPS_API_BASE),
 * по умолчанию — прод. Соцсети проекта задаются здесь же: сайт известен
 * (https://kraimaps.ru), ссылки Telegram/ВК уточнить у владельца проекта.
 */

export const KRAIMAPS_SITE_URL =
  import.meta.env.VITE_KRAIMAPS_SITE_URL ?? 'https://kraimaps.ru';

export const KRAIMAPS_API_BASE =
  import.meta.env.VITE_KRAIMAPS_API_BASE ?? 'https://kraimaps.ru/api/v1';

/**
 * CDN-база публичных изображений (аватары и т.п.).
 * Зеркало NUXT_PUBLIC_CDN_MEDIA_BASE сайта (прод: https://files.kraimaps.ru).
 * Бэкенд отдаёт сырые S3-ключи — полный URL собирает `resolveMediaUrl`.
 */
export const KRAIMAPS_MEDIA_BASE =
  import.meta.env.VITE_KRAIMAPS_MEDIA_BASE ?? 'https://files.kraimaps.ru';

export const OAUTH_CLIENT_ID = 'kraimaps-installer';
export const OAUTH_REDIRECT_URI = 'kraimaps://oauth/callback';
export const OAUTH_SCOPES = '';

export interface SocialLink {
  id: 'site' | 'telegram' | 'vk';
  label: string;
  url: string;
}

/**
 * TODO(owner): подставить реальные ссылки Telegram и ВК проекта.
 * Сайт известен, остальные — заглушки до уточнения (ничего не выдумываем).
 */
export const SOCIAL_LINKS: SocialLink[] = [
  { id: 'site', label: 'Сайт KraiMaps', url: KRAIMAPS_SITE_URL },
  { id: 'telegram', label: 'Telegram', url: 'https://t.me/kraimaps' },
  { id: 'vk', label: 'ВК', url: 'https://vk.ru/kraimaps' },
];

import pkg from '../../package.json';

/**
 * Запасная версия для `pnpm dev` без Tauri-рантайма (invoke недоступен).
 * В проде фронт запрашивает версию у Rust через `api.getInstallerVersion()`
 * (single source — `site::INSTALLER_VERSION` из `Cargo.toml`).
 * `package.json` бампается вместе с `Cargo.toml`/`tauri.conf.json` при релизе.
 */
export const INSTALLER_VERSION_FALLBACK: string = pkg.version;
