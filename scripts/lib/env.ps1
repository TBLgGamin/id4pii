$ErrorActionPreference = 'Stop'

function Read-EnvFile {
  [CmdletBinding()]
  param([Parameter(Mandatory)][string]$Path)

  if (-not (Test-Path -LiteralPath $Path)) {
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

function Require-EnvKey {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][hashtable]$Values,
    [Parameter(Mandatory)][string]$Key
  )
  if (-not $Values.ContainsKey($Key)) {
    throw "missing key '$Key' in .env (see .env.example)"
  }
  return $Values[$Key]
}

function Get-EnvKey {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory)][hashtable]$Values,
    [Parameter(Mandatory)][string]$Key,
    [string]$Default = ''
  )
  if ($Values.ContainsKey($Key)) { return $Values[$Key] }
  return $Default
}
