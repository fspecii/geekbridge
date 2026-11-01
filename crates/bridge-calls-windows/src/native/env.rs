//! Read-only facts about the process and the OS: build, contract versions, package identity and
//! the Phone calls privacy capability.
use crate::model::*;
use windows::Foundation::Metadata::ApiInformation;
use windows::Security::Authorization::AppCapabilityAccess::{
    AppCapability, AppCapabilityAccessStatus,
};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Foundation::{
    APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS,
};
use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::core::{HSTRING, PWSTR};

const CALLS_PHONE_CONTRACT: &str = "Windows.ApplicationModel.Calls.CallsPhoneContract";
/// Capability name behind Settings, Privacy & security, Phone calls.
const PHONE_CALL_CAPABILITY: &str = "phoneCall";

fn windows_build() -> Option<u32> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a valid, correctly sized OSVERSIONINFOW that outlives the call.
    let status = unsafe { RtlGetVersion(&mut info) };
    (status.0 >= 0).then_some(info.dwBuildNumber)
}

fn contract_present(major: u16) -> Option<bool> {
    ApiInformation::IsApiContractPresentByMajor(&HSTRING::from(CALLS_PHONE_CONTRACT), major).ok()
}

fn package_identity() -> PackageIdentity {
    let mut len = 0u32;
    // SAFETY: a null buffer with a zero length asks only for the required length.
    let probe = unsafe { GetCurrentPackageFamilyName(&mut len, None) };
    if probe == APPMODEL_ERROR_NO_PACKAGE {
        return PackageIdentity::Absent;
    }
    if probe != ERROR_INSUFFICIENT_BUFFER || len == 0 || len > 1024 {
        return PackageIdentity::Unknown { code: probe.0 };
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` has room for `len` UTF-16 units, as the previous call required.
    let got = unsafe { GetCurrentPackageFamilyName(&mut len, Some(PWSTR(buf.as_mut_ptr()))) };
    if got != ERROR_SUCCESS {
        return PackageIdentity::Unknown { code: got.0 };
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    PackageIdentity::Present {
        family_name: String::from_utf16_lossy(&buf[..end]),
    }
}

fn privacy() -> PrivacyAccess {
    let Ok(capability) = AppCapability::Create(&HSTRING::from(PHONE_CALL_CAPABILITY)) else {
        return PrivacyAccess::Unavailable;
    };
    match capability.CheckAccess() {
        Ok(AppCapabilityAccessStatus::Allowed) => PrivacyAccess::Allowed,
        Ok(AppCapabilityAccessStatus::DeniedByUser) => PrivacyAccess::DeniedByUser,
        Ok(AppCapabilityAccessStatus::DeniedBySystem) => PrivacyAccess::DeniedBySystem,
        Ok(AppCapabilityAccessStatus::UserPromptRequired) => PrivacyAccess::UserPromptRequired,
        Ok(AppCapabilityAccessStatus::NotDeclaredByApp) => PrivacyAccess::NotDeclaredByApp,
        _ => PrivacyAccess::Unavailable,
    }
}

pub(super) fn environment() -> EnvironmentFacts {
    EnvironmentFacts {
        os: OsFacts {
            platform: OsPlatform::Windows,
            build: windows_build(),
        },
        contract: ContractFacts {
            v5: contract_present(5),
            v6: contract_present(6),
            v7: contract_present(7),
        },
        package: package_identity(),
        privacy: privacy(),
    }
}
