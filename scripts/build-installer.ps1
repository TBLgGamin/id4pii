[CmdletBinding()]
param(
  [string]$EnvFile = (Join-Path (Join-Path $PSScriptRoot "..") ".env"),
  [switch]$SkipCargo,
  [switch]$SkipAssetSync
)

$ErrorActionPreference = 'Stop'

function Read-EnvFile([string]$Path) {
  if (-not (Test-Path $Path)) {
    throw ".env not found at $Path. Copy .env.example to .env and fill it in (see CONTRIBUTING.md)."
  }
  $values = @{}
  foreach ($line in Get-Content -LiteralPath $Path) {
    $trim = $line.Trim()
    if ($trim -eq '' -or $trim.StartsWith('#')) { continue }
    $eq = $trim.IndexOf('=')
    if ($eq -lt 1) { continue }
    $key = $trim.Substring(0, $eq).Trim()
    $value = $trim.Substring($eq + 1).Trim()
    if (($value.StartsWith('"') -and $value.EndsWith('"')) -or
        ($value.StartsWith("'") -and $value.EndsWith("'"))) {
      $value = $value.Substring(1, $value.Length - 2)
    }
    $values[$key] = $value
  }
  return $values
}

function Require-Key([hashtable]$Values, [string]$Key) {
  if (-not $Values.ContainsKey($Key)) {
    throw "missing key '$Key' in .env (see .env.example)"
  }
  return $Values[$Key]
}

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $repoRoot

$env_values = Read-EnvFile -Path $EnvFile
foreach ($pair in $env_values.GetEnumerator()) {
  Set-Item -Path ("Env:" + $pair.Key) -Value $pair.Value
}

$extId   = Require-Key $env_values 'ID4PII_PUBLISHED_EXTENSION_ID'
$version = Require-Key $env_values 'ID4PII_APP_VERSION'
$repo    = Require-Key $env_values 'ID4PII_GITHUB_REPO'
$sign    = if ($env_values.ContainsKey('ID4PII_INSTALLER_SIGNTOOL')) { $env_values['ID4PII_INSTALLER_SIGNTOOL'] } else { '' }
$signUn  = if ($env_values.ContainsKey('ID4PII_INSTALLER_SIGN_UNINSTALLER')) { $env_values['ID4PII_INSTALLER_SIGN_UNINSTALLER'] } else { 'no' }

if (-not $SkipCargo) {
  Write-Host "==> cargo build --release -p id4pii-app"
  cargo build --release -p id4pii-app
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

if (-not $SkipAssetSync) {
  Write-Host "==> sync-extension-assets"
  & (Join-Path $PSScriptRoot "sync-extension-assets.ps1")
}

$iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
if (-not (Test-Path $iscc)) {
  $iscc = "${env:ProgramFiles}\Inno Setup 6\ISCC.exe"
}
if (-not (Test-Path $iscc)) {
  throw "Inno Setup 6 not found. Install from https://jrsoftware.org/isdl.php"
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
