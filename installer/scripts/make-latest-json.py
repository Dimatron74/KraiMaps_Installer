#!/usr/bin/env python3
"""Генерация манифестов релиза установщика из собранного NSIS-артефакта.

Вызывается из CI (`.github/workflows/release-installer.yml`) после
`tauri build`: рядом с `*-setup.exe` уже лежит `*.exe.sig` (подпись Ed25519,
`createUpdaterArtifacts: true`).

Пишет два файла (один источник истины — собранный бинарник):

- `latest.json` — формат сайта (`InstallerReleasesResponse` + расширения v2:
  `min_supported_version`, `signature` в asset'е). Едет на S3, раздаётся
  через `GET /installer/releases` и страницу `/installer`.
- `updater.json` — запасной статический формат Tauri (`platforms`),
  если API сайта недоступен (клиент умеет проверяться напрямую
  через `KRAIMAPS_UPDATES_URL`).

Только stdlib. Пример:
    python installer/scripts/make-latest-json.py \
      --version 0.2.0 \
      --windows-exe target/release/bundle/nsis/app-setup.exe \
      --windows-url https://files.kraimaps.ru/installer/0.2.0/app-setup.exe \
      --out-dir out/
"""

import argparse
import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone

CHUNK = 1024 * 1024

# Строгий semver (мажор.минор.патч + опциональные pre/build).
# Клиент игнорирует непарсящиеся версии fail-open'ом — то есть битая строка
# здесь молча отключила бы обновления/hard-block. Поэтому fail-fast в CI.
SEMVER_RE = re.compile(
    r"^v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$"
)


def require_semver(value: str, name: str) -> str:
    """Проверить semver, вернуть без `v`-префикса. Иначе — fail-fast (exit 2)."""
    if not value or not SEMVER_RE.match(value.strip()):
        print(f"error: --{name} {value!r} не semver (нужно напр. 0.2.0)", file=sys.stderr)
        sys.exit(2)
    return value.strip().lstrip("vV")


def require_https_url(value: str, name: str) -> str:
    """URL артефакта — только HTTPS (ТЗ: метаданные и отпечатки не гоняем открыто)."""
    if not value.startswith("https://"):
        print(f"error: --{name} должен быть https://, получено {value!r}", file=sys.stderr)
        sys.exit(2)
    return value


def sha256_of(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(CHUNK), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_sig(exe_path: str) -> str:
    sig_path = exe_path + ".sig"
    if not os.path.isfile(sig_path):
        raise FileNotFoundError(
            f"нет подписи {sig_path}: сборка без createUpdaterArtifacts "
            "или без TAURI_SIGNING_PRIVATE_KEY"
        )
    with open(sig_path, "r", encoding="utf-8") as f:
        sig = f.read().strip()
    if not sig:
        raise ValueError(f"пустая подпись {sig_path}")
    return sig


def build_manifests(
    version: str,
    windows_exe: str,
    windows_url: str,
    min_supported: str | None,
    notes: str | None,
) -> tuple[dict, dict]:
    size = os.path.getsize(windows_exe)
    asset = {
        "os": "windows",
        "url": windows_url,
        "sha256": sha256_of(windows_exe),
        "size_bytes": size,
        "file_name": os.path.basename(windows_exe),
        # Подпись Tauri (Ed25519) — НЕ путать с sha256: sha256 для ручной
        # проверки на странице, signature — для проверки плагином.
        "signature": read_sig(windows_exe),
    }
    latest: dict = {"version": version, "assets": [asset]}
    if min_supported:
        latest["min_supported_version"] = min_supported
    if notes:
        latest["changelog"] = notes

    updater: dict = {
        "version": version,
        "pub_date": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "platforms": {"windows-x86_64": {"url": windows_url, "signature": asset["signature"]}},
    }
    if notes:
        updater["notes"] = notes
    return latest, updater


def main() -> int:
    parser = argparse.ArgumentParser(description="Манифесты релиза установщика")
    parser.add_argument("--version", required=True, help="semver релиза, напр. 0.2.0")
    parser.add_argument("--windows-exe", required=True, help="путь к *-setup.exe")
    parser.add_argument("--windows-url", required=True, help="публичный URL exe на CDN/S3")
    parser.add_argument(
        "--min-supported",
        default="",
        help="min_supported_version; пусто = hard-block нет",
    )
    parser.add_argument("--notes", default="", help="changelog релиза")
    parser.add_argument("--out-dir", required=True, help="куда писать latest.json/updater.json")
    args = parser.parse_args()

    version = require_semver(args.version, "version")
    min_supported = None
    if args.min_supported.strip():
        min_supported = require_semver(args.min_supported, "min-supported")
    url = require_https_url(args.windows_url, "windows-url")
    latest, updater = build_manifests(
        version=version,
        windows_exe=args.windows_exe,
        windows_url=url,
        min_supported=min_supported,
        notes=args.notes.strip() or None,
    )
    os.makedirs(args.out_dir, exist_ok=True)
    for name, doc in (("latest.json", latest), ("updater.json", updater)):
        path = os.path.join(args.out_dir, name)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(doc, f, ensure_ascii=False, indent=2)
            f.write("\n")
        print(f"wrote {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
