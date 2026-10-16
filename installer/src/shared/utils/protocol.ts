/** Разбор kraimaps:// ссылок (deep-link). Без секретов в URL — только id и mode. */
export type ProtocolTarget =
  | { kind: 'install'; mapId: number; mode: string }
  | { kind: 'oauth'; code: string; state: string }
  | { kind: 'unknown' };

export function parseProtocolUrl(raw: string): ProtocolTarget {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { kind: 'unknown' };
  }
  if (url.protocol !== 'kraimaps:') return { kind: 'unknown' };
  // kraimaps://install/123?mode=auto → host "install", path "/123"
  // kraimaps://oauth/callback?code=..&state=.. → host "oauth", path "/callback"
  const host = url.hostname;
  const parts = url.pathname.split('/').filter(Boolean);
  if (host === 'install' && parts.length >= 1) {
    const mapId = Number(parts[0]);
    if (!Number.isInteger(mapId) || mapId <= 0) return { kind: 'unknown' };
    return { kind: 'install', mapId, mode: url.searchParams.get('mode') ?? 'auto' };
  }
  if (host === 'oauth' && parts[0] === 'callback') {
    const code = url.searchParams.get('code') ?? '';
    const state = url.searchParams.get('state') ?? '';
    if (!code || !state) return { kind: 'unknown' };
    return { kind: 'oauth', code, state };
  }
  return { kind: 'unknown' };
}
