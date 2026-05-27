[CmdletBinding()]
param(
  [string]$EnvFile = (Join-Path (Join-Path $PSScriptRoot "..") ".env"),
  [switch]$SkipCargo,
  [switch]$SkipAssetSync
)

$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot "lib\env.ps1")

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $repoRoot

$env_values = Read-EnvFile -Path $EnvFile
foreach ($pair in $env_values.GetEnumerator()) {
  Set-Item -Path ("Env:" + $pair.Key) -Value $pair.Value
}

$extId   = Require-EnvKey -Values $env_values -Key 'ID4PII_PUBLISHED_EXTENSION_ID'
$version = Require-EnvKey -Values $env_values -Key 'ID4PII_APP_VERSION'
$repo    = Require-EnvKey -Values $env_values -Key 'ID4PII_GITHUB_REPO'
$sign    = Get-EnvKey     -Values $env_values -Key 'ID4PII_INSTALLER_SIGNTOOL'
$signUn  = Get-EnvKey     -Values $env_values -Key 'ID4PII_INSTALLER_SIGN_UNINSTALLER' -Default 'no'

if (-not $SkipCargo) {
  Write-Host "==> cargo build --release -p id4pii-app"
  cargo build --release -p id4pii-app
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

if (-not $SkipAssetSync) {
  Write-Host "==> sync-extension-assets"
  & (Join-Path $PSScriptRoot "sync-extension-assets.ps1")
}

function Find-Iscc {
  $candidates = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles}\Inno Setup 6\ISCC.exe"
  )
  foreach ($p in $candidates) { if (Test-Path $p) { return $p } }
  return $null
}

$iscc = Find-Iscc
if (-not $iscc) {
  $hasWinget = $null -ne (Get-Command winget -ErrorAction SilentlyContinue)
  if (-not $hasWinget) {
    throw "Inno Setup 6 not found and winget is unavailable. Install Inno Setup 6 from https://jrsoftware.org/isdl.php"
  }
  Write-Host "==> Installing Inno Setup 6 via winget (one-time)"
  winget install --id JRSoftware.InnoSetup -e --accept-source-agreements --accept-package-agreements --silent
  if ($LASTEXITCODE -ne 0) { throw "winget install JRSoftware.InnoSetup failed (exit $LASTEXITCODE)" }
  $iscc = Find-Iscc
  if (-not $iscc) {
    throw "Inno Setup 6 still not found after winget install. Check winget output above."
  }
}

$isccArgs = @(
  "/DMyAppVersion=$version",
  "/DChromeExtId=$extId",
  "/DGitHubRepo=$repo",
  "/DSignUninstaller=$signUn"
)
if ($sign) { $isccArgs += "/DSignToolCmd=$sign" }
$isccArgs += "installer\id4pii.iss"

Write-Host "==> $iscc $($isccArgs -join ' ')"
& $iscc @isccArgs
if ($LASTEXITCODE -ne 0) { throw "iscc failed" }

Write-Host ""
Write-Host "OK - installer at installer\dist\id4pii-setup.exe"
