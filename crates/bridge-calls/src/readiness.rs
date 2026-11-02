//! Is this computer able to carry a phone call's audio over Bluetooth hands-free?
//!
//! Three situations look identical from the outside ("calls don't sound on this computer") but
//! have different fixes, so they are told apart and explained in one line each:
//!
//! * no Bluetooth adapter at all (nothing to pair with; a USB dongle is the fix),
//! * an adapter, but the phone is not paired/connected as a hands-free audio gateway,
//! * ready: a phone is registered as an HFP audio gateway.
//!
//! Detection is pure over facts so it can be tested with a fake sysfs tree and canned
//! `bluetoothctl` output. Answering over the GeekBridge link works in every case; only the voice
//! staying on the phone differs. See `docs/call-audio-options.md`.

use std::path::Path;

/// Where the next step is written down.
pub const DOC: &str = "docs/call-audio-options.md";
/// HFP audio gateway service class, as `bluetoothctl info` prints it.
const HFP_AG_UUID_PREFIX: &str = "0000111f-";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// No Bluetooth adapter. Call audio cannot come to this computer.
    NoAdapter,
    /// An adapter exists, no phone is connected as a hands-free audio gateway.
    /// `paired` is whether a hands-free-capable phone is already paired (so only connecting is left).
    NoHfpPhone { paired: bool },
    /// A phone is registered as an audio gateway.
    Ready,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub adapters: usize,
    /// Paired devices advertising the HFP audio gateway service; `None` if it could not be read.
    pub paired_hfp_phones: Option<usize>,
    /// Phones the telephony backend has registered.
    pub registered_phones: usize,
}

pub fn classify(f: &Facts) -> Readiness {
    if f.adapters == 0 {
        Readiness::NoAdapter
    } else if f.registered_phones > 0 {
        Readiness::Ready
    } else {
        Readiness::NoHfpPhone {
            paired: f.paired_hfp_phones.is_some_and(|n| n > 0),
        }
    }
}

impl Readiness {
    /// One line: what is wrong and the next step. Calls keep working on the phone either way.
    pub fn line(self) -> String {
        match self {
            Readiness::NoAdapter => format!(
                "Call audio on this computer: no Bluetooth adapter found. Answering still works and the voice stays on the phone; add a USB Bluetooth dongle to hear calls here ({DOC})."
            ),
            Readiness::NoHfpPhone { paired: false } => format!(
                "Call audio on this computer: Bluetooth adapter found, but the phone is not paired as a hands-free device. Pair it in Bluetooth settings and allow calls ({DOC})."
            ),
            Readiness::NoHfpPhone { paired: true } => format!(
                "Call audio on this computer: phone is paired but not connected as a hands-free device. Connect its Hands-free profile ({DOC})."
            ),
            Readiness::Ready => {
                "Call audio on this computer: ready (phone connected as a hands-free device)."
                    .into()
            }
        }
    }
}

/// Count `hciN` controllers under a sysfs class directory (`/sys/class/bluetooth`). Entries such
/// as `hci0:256` (connections) are not controllers. A missing directory means none.
pub fn count_adapters(class_dir: &Path) -> usize {
    std::fs::read_dir(class_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.strip_prefix("hci")
                        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                })
                .count()
        })
        .unwrap_or(0)
}

/// Addresses from `bluetoothctl devices [Paired]` (`Device AA:BB:CC:DD:EE:FF Name`).
pub fn parse_devices(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            (it.next()? == "Device").then_some(())?;
            let a = it.next()?;
            (a.len() == 17 && a.split(':').count() == 6).then(|| a.to_owned())
        })
        .collect()
}

/// Whether `bluetoothctl info <addr>` lists the HFP audio gateway service.
pub fn info_is_hfp_gateway(out: &str) -> bool {
    out.lines().any(|l| {
        let l = l.trim().to_ascii_lowercase();
        l.starts_with("uuid:") && l.contains(HFP_AG_UUID_PREFIX)
    })
}

/// Gather facts with an injectable `bluetoothctl` runner (`None` = not runnable).
pub fn gather(
    class_dir: &Path,
    registered_phones: usize,
    run: &dyn Fn(&[&str]) -> Option<String>,
) -> Facts {
    let adapters = count_adapters(class_dir);
    let paired_hfp_phones = if adapters == 0 || registered_phones > 0 {
        None
    } else {
        run(&["devices", "Paired"]).map(|list| {
            parse_devices(&list)
                .iter()
                .filter(|a| run(&["info", a]).is_some_and(|i| info_is_hfp_gateway(&i)))
                .count()
        })
    };
    Facts {
        adapters,
        paired_hfp_phones,
        registered_phones,
    }
}

#[cfg(target_os = "linux")]
mod host {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    fn bluetoothctl(args: &[&str]) -> Option<String> {
        let mut child = Command::new("bluetoothctl")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(s)) if s.success() => break,
                Ok(Some(_)) | Err(_) => return None,
                Ok(None) if Instant::now() >= end => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let mut s = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take()?, &mut s).ok()?;
        Some(s)
    }

    static CACHE: Mutex<Option<(Instant, Readiness)>> = Mutex::new(None);

    /// Readiness of the real host for the case where no phone is registered. Cached for 15 s
    /// because status is polled by the UI.
    pub fn detect() -> Readiness {
        let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, r)) = *c
            && at.elapsed() < Duration::from_secs(15)
        {
            return r;
        }
        let r = classify(&gather(Path::new("/sys/class/bluetooth"), 0, &bluetoothctl));
        *c = Some((Instant::now(), r));
        r
    }
}
#[cfg(target_os = "linux")]
pub use host::detect;

#[cfg(test)]
mod tests {
    use super::*;

    const PAIRED: &str = "Device AA:BB:CC:DD:EE:01 Pixel 8\nDevice AA:BB:CC:DD:EE:02 Earbuds\n";
    const PHONE_INFO: &str = "Device AA:BB:CC:DD:EE:01 (public)\n\tName: Pixel 8\n\tPaired: yes\n\tUUID: Handsfree Audio Gateway   (0000111f-0000-1000-8000-00805f9b34fb)\n\tUUID: Audio Source              (0000110a-0000-1000-8000-00805f9b34fb)\n";
    const EARBUD_INFO: &str = "Device AA:BB:CC:DD:EE:02 (public)\n\tUUID: Handsfree                 (0000111e-0000-1000-8000-00805f9b34fb)\n\tUUID: Audio Sink                (0000110b-0000-1000-8000-00805f9b34fb)\n";

    fn fake_sysfs(entries: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for e in entries {
            std::fs::create_dir(d.path().join(e)).unwrap();
        }
        d
    }

    fn runner(args: &[&str]) -> Option<String> {
        match args {
            ["devices", "Paired"] => Some(PAIRED.into()),
            ["info", "AA:BB:CC:DD:EE:01"] => Some(PHONE_INFO.into()),
            ["info", "AA:BB:CC:DD:EE:02"] => Some(EARBUD_INFO.into()),
            _ => None,
        }
    }

    #[test]
    fn missing_or_empty_sysfs_means_no_adapter() {
        let none = std::path::Path::new("/nonexistent/geekbridge-sysfs");
        assert_eq!(count_adapters(none), 0);
        let f = gather(none, 0, &runner);
        assert_eq!(classify(&f), Readiness::NoAdapter);
        assert_eq!(
            f.paired_hfp_phones, None,
            "no bluetoothctl without an adapter"
        );
        assert!(Readiness::NoAdapter.line().contains(DOC));
        assert!(Readiness::NoAdapter.line().contains("USB"));
    }

    #[test]
    fn only_hci_controllers_count_not_connections() {
        let d = fake_sysfs(&["hci0", "hci0:256", "hci1", "hcifoo", "hci"]);
        assert_eq!(count_adapters(d.path()), 2);
    }

    #[test]
    fn adapter_without_hfp_phone() {
        let d = fake_sysfs(&["hci0"]);
        let none = |_: &[&str]| Some(String::new());
        let f = gather(d.path(), 0, &none);
        assert_eq!(f.paired_hfp_phones, Some(0));
        assert_eq!(classify(&f), Readiness::NoHfpPhone { paired: false });
        // bluetoothctl missing: still "adapter present, no phone", just not "paired".
        let f = gather(d.path(), 0, &|_| None);
        assert_eq!(classify(&f), Readiness::NoHfpPhone { paired: false });
    }

    #[test]
    fn paired_phone_is_found_by_its_gateway_uuid_and_earbuds_are_ignored() {
        let d = fake_sysfs(&["hci0"]);
        let f = gather(d.path(), 0, &runner);
        assert_eq!(f.paired_hfp_phones, Some(1));
        let r = classify(&f);
        assert_eq!(r, Readiness::NoHfpPhone { paired: true });
        assert!(r.line().contains("not connected"));
    }

    #[test]
    fn registered_phone_is_ready() {
        let d = fake_sysfs(&["hci0"]);
        let r = classify(&gather(d.path(), 1, &runner));
        assert_eq!(r, Readiness::Ready);
        assert!(r.line().contains("ready"));
    }

    #[test]
    fn parsers_ignore_noise() {
        assert_eq!(
            parse_devices("[CHG] foo\nDevice bad Name\n"),
            Vec::<String>::new()
        );
        assert!(!info_is_hfp_gateway("Name: 0000111f-\n"));
    }
}
