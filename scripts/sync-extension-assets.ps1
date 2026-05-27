$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$src = Join-Path $root "assets"
$dst = Join-Path $root "extension\assets"

if (Test-Path $dst) { Remove-Item -Recurse -Force $dst }
New-Item -ItemType Directory -Force -Path "$dst\lock_frames" | Out-Null
New-Item -ItemType Directory -Force -Path "$dst\promotional" | Out-Null
New-Item -ItemType Directory -Force -Path "$dst\providers" | Out-Null

Copy-Item "$src\lock_frames\*.png" -Destination "$dst\lock_frames\" -Force
foreach ($size in 16, 32, 48, 128) {
  Copy-Item "$src\icon-$size.png" -Destination "$dst\icon-$size.png" -Force
}
foreach ($site in 'chatgpt', 'claude', 'gemini') {
  Copy-Item "$src\promotional\$site-pii.webp" -Destination "$dst\promotional\$site-pii.webp" -Force
}
if (Test-Path "$src\providers") {
  Copy-Item "$src\providers\*.svg" -Destination "$dst\providers\" -Force
}

$count = (Get-ChildItem $dst -Recurse -File | Measure-Object).Count
Write-Host "synced $count files into $dst"
