//! Конвертер иконок инстансов в PNG.
//!
//! Сайт хранит изображения в WebP (см. `file_upload_service`: всё пишется
//! с расширением `.webp`), в архивах встречается JPEG/GIF. Лаунчеры PNG
//! понимают все — конвертируем при создании инстанса, тихо (best-effort).
//! Настоящее `.webm`-видео кадром не станет: декодера видео нет и не надо —
//! такая «иконка» молча пропускается, установка идёт дальше.

use std::path::{Path, PathBuf};

/// Гарантированный PNG для иконки инстанса.
/// PNG отдаём как есть — но только если реально декодируется (в хранилище
/// встречается усечённый мусор с правильным расширением); webp/jpeg/gif
/// декодируем и пересохраняем; всё остальное — None.
pub fn ensure_png(src: &Path) -> Option<PathBuf> {
    if !src.is_file() {
        return None;
    }
    // Декодируемость проверяем всегда: passthrough битой «PNG» приводил
    // к немым иконкам в обоих лаунчерах.
    let img = image::open(src).ok()?;
    if src
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    {
        return Some(src.to_path_buf());
    }
    let dest = temp_png_path()?;
    // Animated GIF → первый кадр: image::open так и отдаёт, нас устраивает.
    img.save_with_format(&dest, image::ImageFormat::Png).ok()?;
    if dest.is_file() {
        Some(dest)
    } else {
        None
    }
}

fn temp_png_path() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("kraimaps");
    std::fs::create_dir_all(&dir).ok()?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some(dir.join(format!("icon_cvt_{}_{}.png", std::process::id(), nanos)))
}

/// MIME по magic bytes (расширению не верим — хранилище уже подводило).
fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        Some("image/png")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

/// Data URL иконки для превью в UI (`<img src>` без file-протокола:
/// прямой `file://` WebView блокирует, asset-scope ради превью не тянем).
/// Best-effort: мусор/отсутствие/переросток — None.
pub fn data_url_for(path: &Path) -> Option<String> {
    const MAX_BYTES: u64 = 8 * 1024 * 1024;
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() == 0 || meta.len() > MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let mime = sniff_mime(&bytes)?;
    use base64::Engine;
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-iconcvt-{tag}"))
    }

    fn red_jpg(dir: &Path) -> PathBuf {
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([200, 30, 30]));
        let p = dir.join("site-icon.jpg");
        img.save_with_format(&p, image::ImageFormat::Jpeg).expect("jpg");
        p
    }

    #[test]
    fn png_passes_through_untouched() {
        let root = temp_root("pass");
        let img = image::RgbImage::from_pixel(1, 1, image::Rgb([10, 20, 30]));
        let p = root.join("a.png");
        img.save_with_format(&p, image::ImageFormat::Png).expect("png");
        assert_eq!(ensure_png(&p), Some(p));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn truncated_png_header_is_rejected() {
        // Реальный кейс с продакшена: хранилище отдало 4 байта заголовка.
        let root = temp_root("trunc");
        let p = root.join("cut.png");
        std::fs::write(&p, [137, 80, 78, 71]).expect("write");
        assert!(ensure_png(&p).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn jpeg_converts_to_real_png() {
        let root = temp_root("jpg");
        let src = red_jpg(&root);
        let out = ensure_png(&src).expect("converted");
        assert_eq!(out.extension().and_then(|e| e.to_str()), Some("png"));
        let back = image::open(&out).expect("open converted");
        assert_eq!((back.width(), back.height()), (2, 2));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn garbage_is_none_not_panic() {
        let root = temp_root("garbage");
        let p = root.join("fake.webp");
        std::fs::write(&p, b"definitely not an image").expect("write");
        assert!(ensure_png(&p).is_none());
        assert!(ensure_png(&root.join("missing.png")).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn data_url_roundtrips_png() {
        let root = temp_root("dataurl");
        let img = image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3]));
        let p = root.join("a.png");
        img.save_with_format(&p, image::ImageFormat::Png).expect("png");
        let url = data_url_for(&p).expect("data url");
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(data_url_for(&root.join("missing.png")).is_none());
        let garbage = root.join("g.bin");
        std::fs::write(&garbage, [137, 80, 78, 71]).expect("write");
        assert!(data_url_for(&garbage).is_none());
        std::fs::remove_dir_all(&root).ok();
    }
}
