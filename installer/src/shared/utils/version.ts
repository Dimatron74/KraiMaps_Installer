/**
 * Зеркало `is_exact_version` из src-tauri/src/services/versions.rs.
 * Точная версия ("40.2.17", "0.19.5") vs диапазон/метка ("[40,)", "latest").
 * Только для отображения — источником истины остаётся бэкенд.
 */
export function isExactVersion(hint: string): boolean {
  const h = hint.trim();
  return (
    h.length > 0 &&
    /[0-9]/.test(h) &&
    /^[A-Za-z0-9._-]+$/.test(h) &&
    h.toLowerCase() !== 'latest'
  );
}
