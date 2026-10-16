/**
 * Резолв медиа-путей сайта в полный URL.
 * Зеркало `getFileUrl` из kraimaps-site/frontend/utils/file-url.ts:
 * бэкенд (`GET /auth/me`) отдаёт сырой S3-ключ (`/images/avatars/...`,
 * legacy `/uploads/...`), а не URL — в WebView установщика относительный
 * src резолвится против origin приложения и даёт 404.
 */
import { KRAIMAPS_MEDIA_BASE } from '../config';

export function resolveMediaUrl(path: string | null | undefined): string | null {
  if (!path) return null;
  const trimmed = path.trim();
  if (!trimmed) return null;

  // Уже абсолютный URL — как есть.
  if (trimmed.startsWith('http://') || trimmed.startsWith('https://')) {
    return trimmed;
  }

  // Нормализация S3-ключа как на сайте: ведущие `/`, дубли `//`, legacy `uploads/`.
  let clean = trimmed.replace(/\\/g, '/').replace(/^\/+/, '').replace(/\/+/g, '/');
  if (clean.startsWith('uploads/')) {
    clean = clean.slice('uploads/'.length);
  }
  while (clean.includes('//')) {
    clean = clean.replace('//', '/');
  }
  if (!clean) return null;

  // Без CDN-базы относительный URL в WebView битый — лучше fallback с буквой.
  const base = KRAIMAPS_MEDIA_BASE.trim().replace(/\/+$/, '');
  if (!base) return null;
  return `${base}/${clean}`;
}
