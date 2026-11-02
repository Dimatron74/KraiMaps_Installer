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

/**
 * Числовые компоненты версии MC (`"1.20.1"` → `[1, 20, 1]`); снапшоты и
 * мусор — `null`. Зеркало `mc_version_parts` в domain/mod.rs: угадывать
 * версию запрещено, поэтому неизвестная версия не сравнивается.
 */
function mcVersionParts(v: string): number[] | null {
  const s = v.trim().replace(/^[vV]/, '');
  if (!s) return null;
  const parts = s.split('.');
  const nums: number[] = [];
  for (const p of parts) {
    if (!/^\d+$/.test(p)) return null;
    nums.push(Number(p));
  }
  return nums;
}

/**
 * Сравнение версий MC по числам (не строкам): `-1`, `0`, `1`.
 * `null` — хотя бы одна версия не распознана.
 */
export function compareMcVersions(a: string, b: string): number | null {
  const pa = mcVersionParts(a);
  const pb = mcVersionParts(b);
  if (!pa || !pb) return null;
  const len = Math.max(pa.length, pb.length);
  for (let i = 0; i < len; i += 1) {
    const x = pa[i] ?? 0;
    const y = pb[i] ?? 0;
    if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}

/** Версия MC известна (числовая) — можно сравнивать и резолвить слой. */
export function isKnownMcVersion(v: string | null | undefined): v is string {
  return !!v && mcVersionParts(v) !== null;
}
