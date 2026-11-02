# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
  Remove the DEVELOPMENT sparse package registered by register-dev.ps1.

.DESCRIPTION
  Untested (authored without a Windows machine). Removes only the package named by -IdentityName.
  It does NOT unregister the app from any phone's Bluetooth transport: do that first, explicitly,
  with `geekbridge-calls-windows unregister --device D --execute`, because that registration is
  stored per user by Windows. It never touches other apps or other phones.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param([string]$IdentityName = 'GeekBridge.Calls.Dev')
$ErrorActionPreference = 'Stop'
if ($IdentityName -notmatch '^[A-Za-z0-9][A-Za-z0-9.\-]{2,49}$') { throw 'IdentityName has invalid characters.' }
$pkg = Get-AppxPackage -Name $IdentityName
if (-not $pkg) { Write-Host "No package named $IdentityName is registered."; return }
if ($PSCmdlet.ShouldProcess($pkg.PackageFullName, 'Remove-AppxPackage')) {
  Remove-AppxPackage -Package $pkg.PackageFullName
}
