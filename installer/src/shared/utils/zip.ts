/**
 * Проверка расширения до похода в Rust: `analyze_pack` открывает любой файл
 * как zip и на не-zip отдаст `ZipCorrupt` — это экран ошибки вместо
 * понятной подсказки «нужен архив».
 */
export function isZipPath(path: string): boolean {
  return path.toLowerCase().endsWith('.zip');
}
