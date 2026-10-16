/**
 * Типы контракта «сайт ↔ установщик».
 * Скопированы дословно из kraimaps-site/backend/schemas/installer.py.
 * Источник истины — бэкенд; docs/установщик.md §§6.3–6.6 устарел, не использовать.
 * Имена полей — snake_case как в API.
 */

export type EntitlementReason = 'ok' | 'paid_external' | 'no_file';

export interface EntitlementResponse {
  map_id: number;
  title: string;
  allowed: boolean;
  reason: EntitlementReason;
}

export interface ManifestVersions {
  primary: string;
  supported: string[];
}

export type ManifestFileKind = 'zip' | 'mrpack';

export interface ManifestFile {
  url: string;
  sha256: string | null;
  size_bytes: number | null;
  kind: ManifestFileKind;
  file_name: string | null;
  expires_at: number | null;
}

export interface ManifestLoader {
  slug: string;
  version: string | null;
}

export interface ManifestIcon {
  url: string;
  sha256: string | null;
}

export interface InstallManifest {
  map_id: number;
  title: string;
  description: string | null;
  minecraft: ManifestVersions;
  file: ManifestFile;
  loader: ManifestLoader | null;
  icon: ManifestIcon | null;
}

export interface InstallStartRequest {
  map_id: number;
  file_sha256?: string | null;
  idempotency_key?: string | null;
  installer_version?: string | null;
  os?: string | null;
  target_kind?: string | null;
}

export interface InstallStartResponse {
  idempotency_key: string;
  is_first: boolean;
}

export type InstallCompleteStatus = 'completed' | 'failed';

export interface InstallCompleteRequest {
  idempotency_key: string;
  status: InstallCompleteStatus;
  error_code?: string | null;
  duration_ms?: number | null;
}

/** Профиль из GET /auth/me (нужен минимум для шапки: ник + аватар). */
export interface SiteUser {
  id: number;
  username: string;
  avatar_url: string | null;
  is_trusted_author: boolean;
}

export interface UserProfileResponse {
  user: SiteUser | null;
  is_authenticated: boolean;
  balance?: number | null;
}

/** Токены native-сессии (POST /auth/oauth/token). В JS не храним — только Rust/keyring. */
export interface NativeTokenResponse {
  access_token: string;
  refresh_token: string;
  token_type: string;
  expires_in: number;
}
