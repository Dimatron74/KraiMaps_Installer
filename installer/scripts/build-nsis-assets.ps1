<#
.SYNOPSIS
  Сборка растровых панелей (BMP) для NSIS-инсталлятора из фирменных лого KraiMaps.

.DESCRIPTION
  NSIS (MUI2) умеет показывать на страницах установщика только растровые изображения
  в формате BMP, причём без альфа-канала: прозрачные пиксели превращаются в чёрные
  прямоугольники. Панели собираются так:

    1. Лого обрезается по границе непрозрачных пикселей (исходники хранят прозрачные поля).
    2. На холсте рисуется фирменный вертикальный градиент #0a1020 -> #050810.
    3. Лого вписывается в заданную область с сохранением пропорций.

  Новых изображений не создаётся: используются только файлы из assets/.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts/build-nsis-assets.ps1
#>
[CmdletBinding()]
param(
  [string]$AssetsDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

if (-not $AssetsDir) {
  $AssetsDir = Join-Path $PSScriptRoot '..\src-tauri\nsis\assets'
}
$AssetsDir = (Resolve-Path $AssetsDir).Path

$BrandTop = [System.Drawing.Color]::FromArgb(0x0a, 0x10, 0x20)
$BrandBottom = [System.Drawing.Color]::FromArgb(0x05, 0x08, 0x10)

function Get-ContentBounds {
  <#
    .SYNOPSIS
      Прямоугольник непрозрачной части изображения.
  #>
  param([System.Drawing.Bitmap]$Bitmap)

  $rect = [System.Drawing.Rectangle]::new(0, 0, $Bitmap.Width, $Bitmap.Height)
  $locked = $Bitmap.LockBits(
    $rect,
    [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
    [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
  )
  try {
    $buffer = New-Object byte[] ($locked.Stride * $Bitmap.Height)
    [System.Runtime.InteropServices.Marshal]::Copy($locked.Scan0, $buffer, 0, $buffer.Length)
  }
  finally {
    $Bitmap.UnlockBits($locked)
  }

  $minX = $Bitmap.Width
  $minY = $Bitmap.Height
  $maxX = -1
  $maxY = -1
  for ($y = 0; $y -lt $Bitmap.Height; $y++) {
    $rowStart = $y * $locked.Stride
    for ($x = 0; $x -lt $Bitmap.Width; $x++) {
      if ($buffer[$rowStart + $x * 4 + 3] -le 8) { continue }
      if ($x -lt $minX) { $minX = $x }
      if ($x -gt $maxX) { $maxX = $x }
      if ($y -lt $minY) { $minY = $y }
      if ($y -gt $maxY) { $maxY = $y }
    }
  }

  if ($maxX -lt 0) { throw 'Изображение полностью прозрачное' }
  return [System.Drawing.Rectangle]::new($minX, $minY, $maxX - $minX + 1, $maxY - $minY + 1)
}

function New-BrandCanvas {
  <#
    .SYNOPSIS
      Холст с вертикальным фирменным градиентом (без альфа-канала).
  #>
  param([int]$Width, [int]$Height)

  $canvas = [System.Drawing.Bitmap]::new($Width, $Height, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
  $graphics = [System.Drawing.Graphics]::FromImage($canvas)
  try {
    $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
    $rect = [System.Drawing.Rectangle]::new(0, 0, $Width, $Height)
    $brush = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
      $rect, $BrandTop, $BrandBottom, [System.Drawing.Drawing2D.LinearGradientMode]::Vertical
    )
    try { $graphics.FillRectangle($brush, $rect) }
    finally { $brush.Dispose() }
  }
  finally {
    $graphics.Dispose()
  }
  return $canvas
}

function Write-BrandPanel {
  <#
    .SYNOPSIS
      Собирает одну панель: градиент + лого-знак + лого-надпись, и сохраняет её в BMP.

    .DESCRIPTION
      Лого вставляется в натуральном размере, без масштабирования: MUI2 масштабирует
      картинку алгоритмом nearest neighbour, и любой ресайз превращает сглаженные края
      лого в «лесенку». Поэтому берём те размеры лого, которые уже отрисованы с сайта.
  #>
  param(
    [string]$Path,
    [int]$Width,
    [int]$Height,
    [string]$MarkFile,
    [string]$WordmarkFile,
    [int]$Gap,
    [ValidateSet('Vertical', 'Horizontal')]
    [string]$Orientation = 'Vertical'
  )

  $mark = [System.Drawing.Image]::FromFile((Join-Path $AssetsDir $MarkFile))
  $wordmark = if ($WordmarkFile) { [System.Drawing.Image]::FromFile((Join-Path $AssetsDir $WordmarkFile)) } else { $null }
  try {
    # Вставляем картинку целиком, вместе с её прозрачными полями: так пиксели
    # остаются «родными» (1:1) и не проходят через ресайз, а поля — часть
    # композиции исходника.
    $markBounds = [System.Drawing.Rectangle]::new(0, 0, $mark.Width, $mark.Height)
    $wordmarkBounds = if ($wordmark) { [System.Drawing.Rectangle]::new(0, 0, $wordmark.Width, $wordmark.Height) } else { [System.Drawing.Rectangle]::new(0, 0, 0, 0) }
    $gap = if ($wordmark) { $Gap } else { 0 }

    if ($Orientation -eq 'Vertical') {
      $groupHeight = $markBounds.Height + $gap + $wordmarkBounds.Height
      $groupTop = [int](($Height - $groupHeight) / 2)
      $markRect = [System.Drawing.Rectangle]::new(
        [int](($Width - $markBounds.Width) / 2), $groupTop, $markBounds.Width, $markBounds.Height
      )
      $wordmarkRect = [System.Drawing.Rectangle]::new(
        [int](($Width - $wordmarkBounds.Width) / 2),
        $groupTop + $markBounds.Height + $gap,
        $wordmarkBounds.Width,
        $wordmarkBounds.Height
      )
    }
    else {
      $groupWidth = $markBounds.Width + $gap + $wordmarkBounds.Width
      $groupLeft = [int](($Width - $groupWidth) / 2)
      $markRect = [System.Drawing.Rectangle]::new(
        $groupLeft, [int](($Height - $markBounds.Height) / 2), $markBounds.Width, $markBounds.Height
      )
      $wordmarkRect = [System.Drawing.Rectangle]::new(
        $groupLeft + $markBounds.Width + $gap,
        [int](($Height - $wordmarkBounds.Height) / 2),
        $wordmarkBounds.Width,
        $wordmarkBounds.Height
      )
    }

    $canvas = New-BrandCanvas -Width $Width -Height $Height
    try {
      $graphics = [System.Drawing.Graphics]::FromImage($canvas)
      try {
        $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceOver
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias

        $markSrc = [System.Drawing.Rectangle]::new(
          $markBounds.X, $markBounds.Y, $markBounds.Width, $markBounds.Height
        )
        $graphics.DrawImage($mark, $markRect, $markSrc, [System.Drawing.GraphicsUnit]::Pixel)
        if ($wordmark) {
          $wordmarkSrc = [System.Drawing.Rectangle]::new(
            $wordmarkBounds.X, $wordmarkBounds.Y, $wordmarkBounds.Width, $wordmarkBounds.Height
          )
          $graphics.DrawImage($wordmark, $wordmarkRect, $wordmarkSrc, [System.Drawing.GraphicsUnit]::Pixel)
        }
      }
      finally {
        $graphics.Dispose()
      }

      $canvas.Save($Path, [System.Drawing.Imaging.ImageFormat]::Bmp)
      Write-Host ("{0}: {1}x{2} (знак {3}x{4}, надпись {5}x{6}) -> {7}" -f
        (Split-Path $Path -Leaf), $Width, $Height,
        $markBounds.Width, $markBounds.Height,
        $wordmarkBounds.Width, $wordmarkBounds.Height,
        $Path)
    }
    finally {
      $canvas.Dispose()
    }
  }
  finally {
    $mark.Dispose()
    if ($wordmark) { $wordmark.Dispose() }
  }
}

# Страница «Приветствие» и «Готово»: MUI2 показывает панель в блоке 195x386 px
# (при 100% DPI) и рисует картинку в него 1:1, поэтому размер совпадает, а лого
# вставляется в натуральном размере (сайтовские PNG — векторные рендеры, любой
# ресайз даёт «лесенку»).
Write-BrandPanel -Path (Join-Path $AssetsDir 'sidebar.bmp') -Width 195 -Height 386 `
  -MarkFile 'logo-mark-128.png' -WordmarkFile 'logo-full-128.png' -Gap 18 -Orientation Vertical
