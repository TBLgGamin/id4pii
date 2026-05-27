[CmdletBinding()]
param(
  [string]$EnvFile = (Join-Path $PSScriptRoot ".." ".env"),
  [string]$OutDir  = (Join-Path $PSScriptRoot ".." "dist")
)

$ErrorActionPreference = 'Stop'
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$src = Join-Path $repoRoot "extension"

& (Join-Path $PSScriptRoot "sync-extension-assets.ps1")

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

$env_values = Read-EnvFile -Path $EnvFile
$installerUrl = Require-Key $env_values 'ID4PII_INSTALLER_URL'
$version      = Require-Key $env_values 'ID4PII_APP_VERSION'

$staging = Join-Path $OutDir "extension-staging"
if (Test-Path $staging) { Remove-Item -Recurse -Force $staging }
New-Item -ItemType Directory -Force -Path $staging | Out-Null
Copy-Item -Recurse "$src\*" $staging -Force

$onboarding = Join-Path $staging "onboarding.js"
$content = Get-Content -Raw -LiteralPath $onboarding
$content = $content.Replace('__ID4PII_INSTALLER_URL__', $installerUrl)
Set-Content -LiteralPath $onboarding -Value $content -Encoding utf8

$manifestPath = Join-Path $staging "manifest.json"
$manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
$manifest.version = $version
$manifest | ConvertTo-Json -Depth 32 | Set-Content -LiteralPath $manifestPath -Encoding utf8

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$zipName = "id4pii-extension-v$version.zip"
$zipPath = Join-Path $OutDir $zipName
if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
Compress-Archive -Path (Join-Path $staging "*") -DestinationPath $zipPath

Write-Host "OK - extension zip at $zipPath"
