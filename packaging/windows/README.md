# GeekBridge Calls: Windows development packaging

Everything here supports **development and diagnosis** of the Windows 11 calling adapter
(`crates/bridge-calls-windows`). It is not a release pipeline.

> **Status.** Authored on Linux. Nothing in this directory has been run on Windows: the manifest has
> not been through `MakeAppx`, the scripts have not been executed, no package has been installed,
> and no Windows API call has been made against a real phone. There is no production signing
> claim, and Microsoft has not approved GeekBridge for any restricted capability.

## What the package declares

`AppxManifest.template.xml` is a **sparse package** manifest (the executable stays outside the
package, in an "external location"). It declares GeekBridge's own placeholder development identity
(`GeekBridge.Calls.Dev`, publisher `CN=GeekBridge Development`) and these capabilities:

| Capability | Kind | Why (public documentation) |
| --- | --- | --- |
| `phoneCall` | general (`uap`) | `PhoneLine`, `PhoneCall`, `PhoneCallManager` |
| `phoneLineTransportManagement` | **restricted** (`rescap`) | `PhoneLineTransportDevice.RequestAccessAsync`, `RegisterApp`, `ConnectAsync` |
| `runFullTrust` | restricted (`rescap`) | a Win32 executable with package identity |

Target: `Windows.Desktop` 10.0.22000.0 and later. Windows 10 is reported unsupported by the adapter;
no Limited Access Feature key or attestation is used or supplied.

The `windows.phoneCallActivation` extension (`uap13`, build 22000 and later) is available behind
`register-dev.ps1 -DeclarePhoneCallActivation`, off by default because the developer CLI does not
handle activations (see `phoneCallActivation.fragment.xml`).

## Developer registration (loose manifest, needs Developer Mode)

On a Windows 11 test PC with the phone already paired in Windows Bluetooth settings and
Settings > Privacy & security > Phone calls enabled for apps:

```powershell
# 1. Build (on Windows)
cargo build --release -p bridge-calls-windows        # -> target\release\geekbridge-calls-windows.exe

# 2. Register the development identity for that folder (review the script first)
.\packaging\windows\register-dev.ps1 -ExeDir .\target\release

# 3. Run from the same folder; the default command is read-only
.\target\release\geekbridge-calls-windows.exe                 # diagnose
.\target\release\geekbridge-calls-windows.exe --json --reveal # full detail for your own device only
```

`diagnose` should report package identity `present`. Controls are separate subcommands that each
need `--execute` (run `geekbridge-calls-windows --help`):

```powershell
geekbridge-calls-windows register --device 1 --execute   # RequestAccessAsync, then RegisterApp
geekbridge-calls-windows connect  --device 1 --execute   # ConnectAsync
```

Registration with Windows is stored **per user and persists** until undone. To reverse:

```powershell
geekbridge-calls-windows unregister --device 1 --execute  # this app, this phone only
.\packaging\windows\unregister-dev.ps1                    # remove the package
```

Neither script nor the adapter ever unregisters another app or another phone.

## Signed package (optional, test PCs only)

Loose registration avoids signing. If you need an actual `.msix`/`.appx` (for example to test
without Developer Mode), `new-dev-certificate.ps1` creates a self-signed development certificate
whose subject equals the manifest publisher, and you can build with `MakeAppx pack` and sign with
`SignTool`. That route is not scripted here because it has not been exercised; a self-signed
certificate is trusted only where you import it and is not a distribution story.

## Known unknowns

* Whether Windows grants `RequestAccessAsync` = `Allowed` to this identity on retail Windows 11 with an
  Android phone, and under which capability set. An access-denied result is reported as a typed
  error and nothing works around it.
* Whether a plain unpackaged process is also accepted (`--allow-unpackaged` runs that experiment).
* Whether registering coexists with other phone-companion apps on the same phone.
* Restricted-capability use in the Microsoft Store would need Partner Center justification; this
  template is for sideloaded development only.
