/**
 * Контракт команд обновлений `updater_check` / `updater_install`
 * (Rust: src-tauri/src/commands.rs). Имена полей — camelCase
 * (serde `rename_all` на стороне Rust).
 */

/** Статус версии: `current` — молчим, `soft` — оффер с отказом, `hard` — блок. */
export type UpdateStatusName = 'current' | 'soft' | 'hard';

export interface UpdateCheckResult {
  status: UpdateStatusName;
  latest: string | null;
  minSupported: string | null;
  notes: string | null;
}

/** События прогресса установки (канал `on_event`, тег `event`). */
export type UpdateDownloadEvent =
  | { event: 'Started'; data: { contentLength: number | null } }
  | { event: 'Progress'; data: { chunkLength: number } }
  | { event: 'Finished' };
