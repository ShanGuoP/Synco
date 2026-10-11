// 从 src-tauri/icons/icon-source.png（Synco 的 S 图形）生成全套图标：
//   src-tauri/icons/icon.png     256 圆角，装进 ICO 当应用图标
//   src-tauri/icons/icon.ico     Windows 安装包与 exe 的图标（PNG 载荷装在 ICO 容器里）
//   public/icons/synco-32.png    浏览器标签页图标
//   public/icons/synco-180.png   加到桌面/主屏时的图块
// 源图不是正方形、也没有 alpha，所以先按长边补成方画布（补的颜色取源图背景），再切圆角。
// 缩放与圆角交给 System.Drawing：这台机上它就能解 PNG、能写 32bppArgb，不必引图像库。
//
//   node tools/make-icon.js [源图路径]
'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ROOT = path.join(__dirname, '..');
const SRC = process.argv[2] || path.join(ROOT, 'src-tauri', 'icons', 'icon-source.png');
if (!fs.existsSync(SRC)) {
  console.error(`找不到源图：${SRC}\n把设计稿放到 src-tauri/icons/icon-source.png 再跑本脚本`);
  process.exit(1);
}

const PS = `
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Drawing
$src = [System.Drawing.Image]::FromFile($env:SRC)
$bg = $src.GetPixel(0,0)
$brush = New-Object System.Drawing.SolidBrush($bg)

function New-Icon($out, $size, $radius) {
  $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $g.Clear([System.Drawing.Color]::Transparent)
  if ($radius -gt 0) {
    $d = $radius * 2
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddArc(0, 0, $d, $d, 180, 90) | Out-Null
    $p.AddArc($size - $d - 1, 0, $d, $d, 270, 90) | Out-Null
    $p.AddArc($size - $d - 1, $size - $d - 1, $d, $d, 0, 90) | Out-Null
    $p.AddArc(0, $size - $d - 1, $d, $d, 90, 90) | Out-Null
    $p.CloseFigure()
    $g.SetClip($p)
    $p.Dispose()
  }
  $g.FillRectangle($brush, 0, 0, $size, $size)
  $k = [double]$size / [math]::Max($src.Width, $src.Height)
  $w = [int][math]::Round($src.Width * $k)
  $h = [int][math]::Round($src.Height * $k)
  $g.DrawImage($src, [int](($size - $w) / 2), [int](($size - $h) / 2), $w, $h)
  $g.Dispose()
  $dir = Split-Path -Parent $out
  if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
  $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  Write-Host ("  写了 " + $out)
}

New-Icon $env:O_ICO256 256 58
New-Icon $env:O_F32 32 8
New-Icon $env:O_F180 180 40
$src.Dispose(); $brush.Dispose()
`;

const out = {
  O_ICO256: path.join(ROOT, 'src-tauri', 'icons', 'icon.png'),
  O_F32: path.join(ROOT, 'public', 'icons', 'synco-32.png'),
  O_F180: path.join(ROOT, 'public', 'icons', 'synco-180.png'),
};
console.log(`Synco 图标生成（源图 ${SRC}）`);
execFileSync('powershell', ['-NoProfile', '-Command', PS], {
  stdio: 'inherit',
  env: { ...process.env, SRC, ...out },
});

// ---- PNG → ICO：Vista 之后 ICO 里直接放 PNG，一张 256 就够所有 DPI 用 -------------
const CRC = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();
function crc32(b) {
  let c = ~0;
  for (let i = 0; i < b.length; i++) c = CRC[(c ^ b[i]) & 0xff] ^ (c >>> 8);
  return ~c;
}
const png = fs.readFileSync(out.O_ICO256);
const head = Buffer.alloc(6);
head.writeUInt16LE(1, 2);
head.writeUInt16LE(1, 4);          // 一张图标
const entry = Buffer.alloc(16);
entry[0] = 0; entry[1] = 0;        // 256 在单字节字段里写作 0
entry.writeUInt16LE(1, 4);
entry.writeUInt16LE(32, 6);
entry.writeUInt32LE(png.length, 8);
entry.writeUInt32LE(22, 12);
const ico = path.join(ROOT, 'src-tauri', 'icons', 'icon.ico');
fs.writeFileSync(ico, Buffer.concat([head, entry, png]));
console.log(`  写了 ${ico}（${fs.statSync(ico).size} 字节）`);
