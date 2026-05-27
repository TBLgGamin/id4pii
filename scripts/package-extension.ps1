[CmdletBinding()]
param(
  [string]$EnvFile = (Join-Path (Join-Path $PSScriptRoot "..") ".env"),
  [string]$OutDir  = (Join-Path (Join-Path $PSScriptRoot "..") "dist")
)

$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot "lib\env.ps1")

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$src = Join-Path $repoRoot "extension"

& (Join-Path $PSScriptRoot "sync-extension-assets.ps1")

$env_values   = Read-EnvFile -Path $EnvFile
$installerUrl = Require-EnvKey -Values $env_values -Key 'ID4PII_INSTALLER_URL'
$version      = Require-EnvKey -Values $env_values -Key 'ID4PII_APP_VERSION'

$staging = Join-Path $OutDir "extension-staging"
if (Test-Path $staging) { Remove-Item -Recurse -Force $staging }
New-Item -ItemType Directory -Force -Path $staging | Out-Null
Copy-Item -Recurse "$src\*" $staging -Force

$onboarding = Join-Path $staging "onboarding\onboarding.js"
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
