/**
 * Установка с сайта: entitlement → manifest → скачивание (Rust) → реюз визарда.
 * Логика визарда не дублируется: скачанный файл отдаём в useInstallStore.pickZip,
 * дальше — те же шаги confirm/launcher/target/deploy. Телеметрия — в install-*.
 */
import { defineStore } from 'pinia';
import { listen } from '@tauri-apps/api/event';
import { useInstallStore } from './install';
import { api } from '../../shared/api/commands';
import { toErrorCode, toErrorDetails, toUserMessage } from '../../shared/utils/errors';
import { INSTALLER_VERSION_FALLBACK } from '../../shared/config';
import type { EntitlementResponse, InstallManifest } from '../../shared/types/site';

export type SiteStep =
  | 'idle'
  | 'checking'
  | 'downloading'
  | 'handing'
  | 'failed';

export interface DownloadProgress {
  done_bytes: number;
  total_bytes: number | null;
}

function detectOs(): string {
  const p = navigator.userAgent.toLowerCase();
  if (p.includes('win')) return 'windows';
  if (p.includes('linux')) return 'linux';
  return 'windows';
}

/**
 * Единая версия установщика из Rust (`get_installer_version`).
 * В `pnpm dev` без Tauri-рантайма — запасная из `package.json`.
 */
async function installerVersion(): Promise<string> {
  try {
    return await api.getInstallerVersion();
  } catch {
    return INSTALLER_VERSION_FALLBACK;
  }
}

export const useSiteInstallStore = defineStore('site-install', {
  state: () => ({
    mapId: null as number | null,
    mode: 'auto' as string,
    step: 'idle' as SiteStep,
    entitlement: null as EntitlementResponse | null,
    manifest: null as InstallManifest | null,
    progress: null as DownloadProgress | null,
    idempotencyKey: null as string | null,
    startedAt: 0,
    error: null as { code: string; message: string; details: string; hint: string } | null,
    /** Разовое уведомление над прогрессом (напр. «Вход выполнен»). */
    notice: null as string | null,
  }),

  actions: {
    reset() {
      this.mapId = null;
      this.mode = 'auto';
      this.step = 'idle';
      this.entitlement = null;
      this.manifest = null;
      this.progress = null;
      this.idempotencyKey = null;
      this.startedAt = 0;
      this.error = null;
      this.notice = null;
    },

    fail(code: string, message: string, details: string, hint: string) {
      this.step = 'failed';
      this.error = { code, message, details, hint };
      this.notice = null;
    },

    /** Точка входа из protocol kraimaps://install/{id}. Требует сессии (auth-only). */
    async startFromSite(mapId: number, mode = 'auto', notice: string | null = null) {
      this.reset();
      this.mapId = mapId;
      this.mode = mode;
      this.notice = notice;
      this.step = 'checking';
      this.startedAt = Date.now();
      try {
        const ent = await api.getEntitlement(mapId);
        this.entitlement = ent;
        if (!ent.allowed) {
          this.fail(
            'E_ACCESS_DENIED',
            'Эта карта недоступна для установки',
            ent.reason,
            ent.reason === 'paid_external'
              ? 'Эта карта продаётся на внешней площадке. Открой страницу карты на сайте.'
              : 'Файл для установки пока недоступен. Попробуй позже или скачай архив вручную.',
          );
          return;
        }
        const manifest = await api.getManifest(mapId);
        this.manifest = manifest;
        await this.downloadAndHandOver(manifest);
      } catch (e) {
        const code = toErrorCode(e);
        const authHint =
          code === 'E_UNAUTHORIZED'
            ? 'Войди в аккаунт, чтобы ставить карты с сайта в один клик.'
            : 'Проверь соединение и попробуй ещё раз.';
        this.fail(code, toUserMessage(e) || 'Не удалось получить данные карты', toErrorDetails(e), authHint);
      }
    },

    async downloadAndHandOver(manifest: InstallManifest) {
      this.step = 'downloading';
      this.progress = { done_bytes: 0, total_bytes: manifest.file.size_bytes };
      const unlisten = await listen<DownloadProgress>('site-download-progress', (ev) => {
        this.progress = ev.payload;
      });
      try {
        const started = await api.installStart({
          map_id: manifest.map_id,
          file_sha256: manifest.file.sha256,
          installer_version: await installerVersion(),
          os: detectOs(),
          target_kind: null,
        });
        this.idempotencyKey = started.idempotency_key;
        const localPath = await api.downloadMapFile(manifest.map_id);
        unlisten();
        this.step = 'handing';
        const install = useInstallStore();
        install.loadRoots();
        await install.pickZip(localPath);
        // Мета манифеста — строго после pickZip (он делает reset стора).
        // Иконку pickZip уже достал из архива сам (общий путь обоих потоков).
        install.setSiteMeta(
          manifest.loader
            ? { loaderSlug: manifest.loader.slug, loaderVersion: manifest.loader.version }
            : null,
          manifest.title,
        );
        // pickZip перевёл визард на confirm/analyzing — дальше пользователь идёт сам.
        this.step = 'idle';
        this.notice = null;
      } catch (e) {
        unlisten();
        this.fail(
          toErrorCode(e),
          toUserMessage(e) || 'Не удалось скачать файл карты',
          toErrorDetails(e),
          'Проверь соединение и попробуй ещё раз. Ссылка на файл действует недолго.',
        );
      }
    },

    /** Вызывать из DoneStep: успешная установка. */
    async reportCompleted(_targetKind: string | null) {
      if (!this.idempotencyKey) return;
      try {
        await api.installComplete({
          idempotency_key: this.idempotencyKey,
          status: 'completed',
          duration_ms: Date.now() - this.startedAt,
        });
      } catch {
        // Телеметрия не должна ломать UI успеха.
      } finally {
        this.idempotencyKey = null;
      }
    },

    async reportFailed(code: string) {
      if (!this.idempotencyKey) return;
      try {
        await api.installComplete({
          idempotency_key: this.idempotencyKey,
          status: 'failed',
          error_code: code.slice(0, 40),
          duration_ms: Date.now() - this.startedAt,
        });
      } catch {
        // Телеметрия не должна ломать UI успеха.
      } finally {
        this.idempotencyKey = null;
      }
    },
  },
});
