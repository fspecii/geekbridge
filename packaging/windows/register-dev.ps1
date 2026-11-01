<#
.SYNOPSIS
  Register GeekBridge's DEVELOPMENT sparse package so geekbridge-calls-windows.exe runs with
  package identity. Registers a loose manifest (needs Developer Mode); makes no change to any
  phone, Bluetooth pairing or other app's registration.

.DESCRIPTION
  Untested: written on Linux without a Windows machine. Review before running. This is a
  development identity, not a production or Store identity, and nothing is signed here.

  Steps: fill AppxManifest.template.xml -> write it with placeholder assets into -OutDir ->
  Add-AppxPackage -Register <manifest> -ExternalLocation <ExeDir>.

  Afterwards run the executable from -ExeDir and check that `geekbridge-calls-windows diagnose`
  reports package identity "present".

.PARAMETER ExeDir
  Folder that contains geekbridge-calls-windows.exe (the external location).
.PARAMETER OutDir
  Where the manifest and assets are written. Default: .\build\sparse
.PARAMETER IdentityName / Publisher / Version
  Development identity. The publisher must match the certificate if you later sign a package.
.PARAMETER DeclarePhoneCallActivation
  Also declare the windows.phoneCallActivation extension (off by default, see the fragment file).
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
  [Parameter(Mandatory = $true)][string]$ExeDir,
  [string]$OutDir = (Join-Path $PSScriptRoot 'build\sparse'),
  [string]$IdentityName = 'GeekBridge.Calls.Dev',
  [string]$Publisher = 'CN=GeekBridge Development',
  [string]$Version = '0.1.0.0',
  [switch]$DeclarePhoneCallActivation
)
$ErrorActionPreference = 'Stop'
$exe = 'geekbridge-calls-windows.exe'

if ([Environment]::OSVersion.Version.Build -lt 22000) {
  throw 'Windows 11 (build 22000 or later) is required. Windows 10 is unsupported.'
}
$ExeDir = (Resolve-Path -LiteralPath $ExeDir).Path
if (-not (Test-Path -LiteralPath (Join-Path $ExeDir $exe))) { throw "$exe not found in $ExeDir" }
if ($IdentityName -notmatch '^[A-Za-z0-9][A-Za-z0-9.\-]{2,49}$') { throw 'IdentityName has invalid characters.' }
if ($Version -notmatch '^\d+\.\d+\.\d+\.\d+$') { throw 'Version must be a.b.c.d' }
if ($Publisher -notmatch '^CN=[^<>"&]+$') { throw 'Publisher must look like CN=Name (no markup characters).' }

$extensions = ''
if ($DeclarePhoneCallActivation) {
  $extensions = (Get-Content -Raw (Join-Path $PSScriptRoot 'phoneCallActivation.fragment.xml')) `
    -replace '(?s)<!--.*?-->\s*', ''
}
$manifest = (Get-Content -Raw (Join-Path $PSScriptRoot 'AppxManifest.template.xml')) `
  -replace '@IDENTITY_NAME@', $IdentityName `
  -replace '@PUBLISHER@', $Publisher `
  -replace '@VERSION@', $Version `
  -replace '@EXTENSIONS@', $extensions `
  -replace '@EXECUTABLE@', $exe

New-Item -ItemType Directory -Force -Path (Join-Path $OutDir 'Assets') | Out-Null
# A 1x1 transparent PNG stands in for the required logo files in a development registration.
$png = [Convert]::FromBase64String('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==')
foreach ($name in 'StoreLogo.png', 'Square150x150Logo.png', 'Square44x44Logo.png') {
  [IO.File]::WriteAllBytes((Join-Path $OutDir "Assets\$name"), $png)
}
$manifestPath = Join-Path $OutDir 'AppxManifest.xml'
Set-Content -LiteralPath $manifestPath -Value $manifest -Encoding UTF8
[xml](Get-Content -Raw $manifestPath) | Out-Null   # well-formedness check only, not schema validation

if ($PSCmdlet.ShouldProcess("$IdentityName at $ExeDir", 'Add-AppxPackage -Register -ExternalLocation')) {
  Add-AppxPackage -Register $manifestPath -ExternalLocation $ExeDir
  Write-Host "Registered $IdentityName. Run $exe from $ExeDir, then: $exe diagnose"
  Write-Host 'Also enable Settings > Privacy & security > Phone calls for apps, and pair the phone in Windows Bluetooth settings.'
}
