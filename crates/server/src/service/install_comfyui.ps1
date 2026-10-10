# 由 Synco 生成，参数都在 manifest.json 里
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = $PSScriptRoot
$m = Get-Content (Join-Path $root 'manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$state = Join-Path $root 'progress.json'
function Save($st, $msg, $done, $total) {
  @{ status = $st; message = $msg; done = $done; total = $total; at = (Get-Date).ToString('s') } |
    ConvertTo-Json -Compress | Set-Content $state -Encoding UTF8
}
$ui = Join-Path $m.comfy_root 'ComfyUI'
$py = Join-Path $m.comfy_root 'python_embeded\python.exe'
# 便携包不在就直接替他装：下载（断点续传）→ 就地解压 → 剥掉外层目录。
# 这一步是几十分钟量级的，所以全程写 progress.json，工坊界面看得见进度。
if (-not (Test-Path (Join-Path $ui 'main.py'))) {
  # 根目录名里连 ComfyUI 都没有，多半是填错了：这种位置闷头塞 12 GB 进去比报错糟。
  if ($m.comfy_root -notmatch 'ComfyUI') {
    Save 'error' ('根目录看着不像 ComfyUI 便携包的位置：' + $m.comfy_root) 0 0
    Write-Host ('这里没有 ComfyUI\main.py，而这个路径名里也不含 ComfyUI，所以脚本没动手下载。' + '先在工坊「ComfyUI 目录」那页把根目录填对（便携包解压出来的 ComfyUI_windows_portable 那一层），再重跑本脚本。') -ForegroundColor Red
    exit 1
  }
  New-Item -ItemType Directory -Force -Path $m.comfy_root | Out-Null
  $drive = (Get-Item $m.comfy_root).PSDrive.Name
  $free = (Get-PSDrive $drive).Free / 1GB
  if ($free -lt 24) {
    Save 'error' ('磁盘剩余空间不够：' + [math]::Round($free, 1) + ' GB') 0 0
    Write-Host ($drive + ': 只剩 ' + [math]::Round($free, 1) + ' GB。便携包解压后约 12 GB，加上下载包本身要 24 GB 以上。' + '要么换个盘装 ComfyUI，要么先清空间——工坊里"ComfyUI 目录"可以改根目录。') -ForegroundColor Red
    exit 1
  }
  $arc = Join-Path $env:TEMP 'comfyui_windows_portable.7z'
  Save 'comfy' '下载 ComfyUI 便携包（约 4 GB，看网速）' 0 3
  Write-Host ('下载 ' + $m.portable_url + ' → ' + $arc) -ForegroundColor Cyan
  $curl = @(if ($m.proxy) { '--proxy'; $m.proxy }) + @('-L', '--fail', '--retry', '8', '--retry-delay', '5', '-C', '-', '-o', $arc, $m.portable_url)
  & curl.exe @curl
  if ($LASTEXITCODE -ne 0) {
    Save 'error' '便携包下载没成功' 0 0
    Write-Host ('curl 退出码 ' + $LASTEXITCODE + '。已下载的部分留在 ' + $arc + '，重跑本脚本会从断点接着下。' + '网络要走代理的话，在工坊"ComfyUI 目录"那页填代理地址。') -ForegroundColor Red
    exit 1
  }
  Save 'comfy' '解压便携包' 1 3
  Write-Host '解压中，这一步几分钟，别关窗口' -ForegroundColor Cyan
  tar.exe -xf $arc -C $m.comfy_root
  if ($LASTEXITCODE -ne 0) {
    $seven = @(Get-Command 7z.exe, 7za.exe -ErrorAction SilentlyContinue) | Select-Object -First 1
    if ($seven) { & $seven.Source x ('-o' + $m.comfy_root) -y $arc | Out-Null; if ($LASTEXITCODE -ne 0) { $seven = $null } }
    if (-not $seven) {
      Save 'error' '解压失败：这台机器的 tar.exe 读不了 7z，也没装 7-Zip' 0 0
      Write-Host ('下载好的包在 ' + $arc + '。装一个 7-Zip 再重跑本脚本，或者自己把它解压到 ' + $m.comfy_root + '。' + '包没删，不用重下。') -ForegroundColor Red
      exit 1
    }
  }
  Save 'comfy' '摆正目录' 2 3
  # 包里套了一层 ComfyUI_windows_portable/，剥掉它；同盘改名，不搬数据
  $inner = Get-ChildItem $m.comfy_root -Directory -Filter 'ComfyUI_windows_portable*' | Select-Object -First 1
  if ($inner) {
    Get-ChildItem $inner.FullName -Force | Move-Item -Destination $m.comfy_root -Force
    Remove-Item $inner.FullName -Recurse -Force
  }
  Remove-Item $arc -Force -ErrorAction SilentlyContinue
  if (-not (Test-Path (Join-Path $ui 'main.py'))) {
    Save 'error' '解压完了但没找到 ComfyUI/main.py' 0 0
    Write-Host ('包的结构和预期不一样，' + $m.comfy_root + ' 里现在有什么请自己看一眼。脚本没删你任何东西。') -ForegroundColor Red
    exit 1
  }
  Save 'comfy' 'ComfyUI 便携包已就位' 3 3
  Write-Host ('ComfyUI 装到 ' + $m.comfy_root) -ForegroundColor Green
}
if (-not (Test-Path $py)) {
  Save 'error' ('没有内嵌 Python：' + $py) 0 0
  Write-Host ('没找到 ' + $py + '。便携包自带 python_embeded，装好它再重跑本脚本。') -ForegroundColor Red
  exit 1
}
$nodes = Join-Path $ui 'custom_nodes'
New-Item -ItemType Directory -Force -Path $nodes | Out-Null
$skipped = @()
$i = 0
foreach ($p in $m.packs) {
  $i++
  $dst = Join-Path $nodes $p.dir
  if (Test-Path (Join-Path $dst '__init__.py')) { Save 'packs' ($p.dir + ' 已装') $i $m.packs.Count; continue }
  if (Test-Path $dst) {
    $skipped += $p.dir
    Save 'packs' ($p.dir + ' 目录在但不是标准安装，没动它') $i $m.packs.Count
    Write-Host ('跳过 ' + $p.dir + '：已有目录可能是手动或 zip 装的，本脚本不删别人的东西。要重装请自己清掉 ' + $dst) -ForegroundColor Yellow
    continue
  }
  Save 'packs' ('克隆 ' + $p.dir) $i $m.packs.Count
  git -c http.proxy=$m.proxy clone --depth 1 $p.git $dst
  if ($LASTEXITCODE -ne 0) { Save 'error' ('克隆失败：' + $p.dir) $i $m.packs.Count; exit 1 }
  $req = Join-Path $dst 'requirements.txt'
  if (Test-Path $req) {
    Save 'packs' ('装依赖：' + $p.dir) $i $m.packs.Count
    & $py -m pip install -r $req $(if ($m.proxy) { "--proxy=$($m.proxy)" })
  }
}
$total = $m.models.Count
$k = 0
$miss = 0
$diff = 0
foreach ($d in $m.models) {
  $k++
  if (-not (Test-Path $d.to)) {
    if ($d.optional) { Save 'models' ($d.label + ' 可选，未启用') $k $total; continue }
    $miss++
    Save 'models' ('缺文件：' + $d.label) $k $total
    Write-Host ('缺文件：' + $d.label + ' → 请自己放到 ' + $d.to) -ForegroundColor Yellow
    continue
  }
  if ($d.sha256) {
    Save 'models' ($d.label + ' 核对指纹中…') $k $total
    $got = (Get-FileHash -Algorithm SHA256 -Path $d.to).Hash.ToLower()
    if ($got -ne $d.sha256) {
      $diff++
      Save 'models' ($d.label + ' 与工坊基准不符') $k $total
      Write-Host ('指纹不符：' + $d.label + '（基准 ' + $d.sha256.Substring(0, 12) + '… / 本机 ' + $got.Substring(0, 12) + '…）。文件原样留着，自己确认是不是换过版本。') -ForegroundColor Yellow
    } else {
      Save 'models' ($d.label + ' 已在位且指纹一致') $k $total
    }
    continue
  }
  $len = (Get-Item $d.to).Length
  if ($d.min_bytes -gt 0 -and $len -lt $d.min_bytes) {
    $diff++
    Save 'models' ($d.label + ' 体积比基准小，可能没传完') $k $total
    Write-Host ('体积偏小：' + $d.label + '（' + $len + ' / 期望 ' + $d.min_bytes + ' 字节）') -ForegroundColor Yellow
  } else {
    Save 'models' ($d.label + ' 已在位（无指纹基准，只核对了体积）') $k $total
  }
}
$tail = '检查完毕'
if ($miss) { $tail = '缺 ' + $miss + ' 个权重，按上面的路径放好后回工坊重新自检' }
elseif ($diff) { $tail = '有 ' + $diff + ' 个文件与基准不符，回工坊看详情' }
if ($skipped.Count) { $tail = $tail + '；跳过的节点包：' + ($skipped -join ', ') }
Save 'done' $tail $total $total
Write-Host $tail -ForegroundColor Green
Read-Host '按回车关闭'
