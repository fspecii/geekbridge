# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
  Create a self-signed DEVELOPMENT code-signing certificate for signing a test MSIX of this package.

.DESCRIPTION
  Untested (authored without a Windows machine). Only needed if you build and sign a package with
  MakeAppx/SignTool instead of using register-dev.ps1's loose registration. The certificate Subject
  must equal the manifest Publisher. Trusting it (-Trust) writes to the machine's TrustedPeople
  store and needs an elevated prompt; do that only on a test PC. This is not a production
  signing story and the certificate is not issued by any trusted authority.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
  [string]$Publisher = 'CN=GeekBridge Development',
  [string]$Export = (Join-Path $PSScriptRoot 'build\geekbridge-dev.cer'),
  [switch]$Trust
)
$ErrorActionPreference = 'Stop'
if ($Publisher -notmatch '^CN=[^<>"&]+$') { throw 'Publisher must look like CN=Name.' }
New-Item -ItemType Directory -Force -Path (Split-Path $Export) | Out-Null
$cert = New-SelfSignedCertificate -Type Custom -Subject $Publisher -KeyUsage DigitalSignature `
  -FriendlyName 'GeekBridge development (not for distribution)' -CertStoreLocation 'Cert:\CurrentUser\My' `
  -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
Export-Certificate -Cert $cert -FilePath $Export | Out-Null
Write-Host "Created $($cert.Thumbprint); public certificate written to $Export"
if ($Trust -and $PSCmdlet.ShouldProcess($Export, 'Import into LocalMachine\TrustedPeople')) {
  Import-Certificate -FilePath $Export -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null
  Write-Host 'Trusted for this machine (test PCs only).'
}
