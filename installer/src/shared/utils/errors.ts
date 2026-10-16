/**
 * Ошибки Tauri-команд: Rust возвращает `Err(CmdError {code, message, details})`
 * (см. src-tauri/src/commands.rs), а `invoke()` реджектится plain-объектом —
 * НЕ экземпляром Error. Наивный `String(e)` даёт "[object Object]".
 */

export interface CmdErrorShape {
  code?: string;
  message?: string;
  details?: string;
}

/** Человекочитаемый текст для UI (поле message или fallback). */
export function toUserMessage(e: unknown): string {
  if (e instanceof Error && e.message) return e.message;
  if (typeof e === 'string' && e) return e;
  if (e !== null && typeof e === 'object') {
    const o = e as CmdErrorShape;
    if (typeof o.message === 'string' && o.message) return o.message;
  }
  return 'Неизвестная ошибка';
}

/** Технические детали для «Подробнее» (код + details). */
export function toErrorDetails(e: unknown): string {
  if (e !== null && typeof e === 'object') {
    const o = e as CmdErrorShape;
    const parts: string[] = [];
    if (typeof o.code === 'string' && o.code) parts.push(`Код: ${o.code}`);
    if (typeof o.details === 'string' && o.details) parts.push(o.details);
    if (parts.length > 0) return parts.join('\n');
  }
  if (e instanceof Error) return e.stack ?? e.message;
  return String(e);
}

/** Код ошибки (E_*) для телеметрии/ветвлений, если есть. */
export function toErrorCode(e: unknown): string {
  if (e !== null && typeof e === 'object') {
    const o = e as CmdErrorShape;
    if (typeof o.code === 'string' && o.code) return o.code;
  }
  return 'E_UNKNOWN';
}

/**
 * Сессия доказано мертва — можно показывать экран входа.
 * Единственный фатальный код — `E_UNAUTHORIZED` (Rust: `AuthRequired`,
 * мёртвый refresh с 401). Всё остальное (сеть, 503, 429, `E_UNKNOWN`) —
 * транзиент: гостем не объявлять, статус сохранять (I-3).
 */
export function isSessionDeadCode(code: string): boolean {
  return code === 'E_UNAUTHORIZED';
}
