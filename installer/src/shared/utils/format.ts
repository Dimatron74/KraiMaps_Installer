/** Человекочитаемые байты для подписей прогресса («12,4 из 48,1 МБ»). */
export function formatBytes(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n) || n < 0) return '—';
  if (n < 1024) return `${Math.round(n)} Б`;
  const kb = n / 1024;
  if (kb < 1024) return `${trim(kb)} КБ`;
  const mb = kb / 1024;
  if (mb < 1024) return `${trim(mb)} МБ`;
  return `${trim(mb / 1024)} ГБ`;
}

function trim(v: number): string {
  // Одна десятая, запятая как в русской локали: «12,4».
  return (Math.round(v * 10) / 10).toString().replace('.', ',');
}
