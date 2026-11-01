//! `geekbridge-phone-sim`: serve a simulated phone as `org.pipewire.Telephony` on D-Bus, backed
//! by an Android emulator (via `adb`) or an in-memory fake modem. Signalling only: no Bluetooth,
//! no audio.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "geekbridge-phone-sim: unsupported platform: the simulated org.pipewire.Telephony service \
         needs Linux and D-Bus"
    );
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(linux::run());
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::BufRead;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use bridge_phone_sim::{
        AdbModem, FakeModem, Modem, RemoteParty, Sim, SimConfig, SimError, redact_numbers,
    };

    const HELP: &str = "\
geekbridge-phone-sim: simulated phone serving org.pipewire.Telephony (signalling only)

USAGE: geekbridge-phone-sim [OPTIONS]

  --bus ADDR         D-Bus address to serve on (default: the session bus)
  --adb PATH         adb executable (default: adb)
  --serial S         adb device serial (default: emulator-5584)
  --adb-port N       adb server port (passed as `adb -P N`)
  --fake             use the in-memory fake modem instead of adb; reads commands on stdin:
                       ring N | accept N | cancel N | state     (N = remote number)
  --interval-ms N    modem poll interval in milliseconds (default: 300)
  --phone-address A  Bluetooth address to report (default: 02:00:00:00:00:01)
  --help             show this help

Fails if org.pipewire.Telephony is already owned (for example by WirePlumber on the
session bus); pass --bus with a private bus address in that case.";

    static STOP: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_signal(_: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }

    fn install_signal_handlers() {
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
            libc::signal(libc::SIGHUP, handler);
        }
    }

    struct Args {
        bus: Option<String>,
        adb: PathBuf,
        serial: String,
        adb_port: Option<u16>,
        fake: bool,
        interval: Duration,
        phone_address: Option<String>,
    }

    fn parse_args() -> Result<Option<Args>, String> {
        let mut a = Args {
            bus: None,
            adb: PathBuf::from("adb"),
            serial: "emulator-5584".into(),
            adb_port: None,
            fake: false,
            interval: Duration::from_millis(300),
            phone_address: None,
        };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
            match arg.as_str() {
                "--help" | "-h" => return Ok(None),
                "--bus" => a.bus = Some(value("--bus")?),
                "--adb" => a.adb = PathBuf::from(value("--adb")?),
                "--serial" => a.serial = value("--serial")?,
                "--adb-port" => {
                    a.adb_port = Some(
                        value("--adb-port")?
                            .parse()
                            .map_err(|_| "--adb-port must be a port number".to_string())?,
                    )
                }
                "--fake" => a.fake = true,
                "--interval-ms" => {
                    let ms: u64 = value("--interval-ms")?
                        .parse()
                        .map_err(|_| "--interval-ms must be a number".to_string())?;
                    if ms == 0 {
                        return Err("--interval-ms must be at least 1".into());
                    }
                    a.interval = Duration::from_millis(ms);
                }
                "--phone-address" => a.phone_address = Some(value("--phone-address")?),
                other => return Err(format!("unknown argument {other:?} (try --help)")),
            }
        }
        Ok(Some(a))
    }

    /// Tiny stdin console for `--fake`, so the demo can be driven by hand.
    fn fake_console(fake: FakeModem) {
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                let mut w = line.split_whitespace();
                let res = match (w.next(), w.next()) {
                    (Some("ring"), Some(n)) => fake.ring(n),
                    (Some("accept"), Some(n)) => fake.remote_accept(n),
                    (Some("cancel"), Some(n)) => fake.remote_hangup(n),
                    (Some("state"), None) => {
                        eprintln!(
                            "fake modem: {:?}",
                            fake.state().map(|s| s.number().is_some())
                        );
                        continue;
                    }
                    (None, _) => continue,
                    _ => {
                        eprintln!("commands: ring N | accept N | cancel N | state");
                        continue;
                    }
                };
                if let Err(e) = res {
                    eprintln!("fake modem: {}", redact_numbers(&e.to_string()));
                }
            }
        });
    }

    pub fn run() -> i32 {
        let args = match parse_args() {
            Ok(Some(a)) => a,
            Ok(None) => {
                println!("{HELP}");
                return 0;
            }
            Err(e) => {
                eprintln!("geekbridge-phone-sim: {e}");
                return 2;
            }
        };
        let (modem, label): (Arc<dyn Modem>, String) = if args.fake {
            let fake = FakeModem::new();
            fake_console(fake.clone());
            (Arc::new(fake), "fake".to_string())
        } else {
            (
                Arc::new(AdbModem::new(
                    args.adb.clone(),
                    args.serial.clone(),
                    args.adb_port,
                )),
                format!("adb serial={}", args.serial),
            )
        };
        let mut cfg = SimConfig {
            bus_address: args.bus.clone(),
            poll_interval: args.interval,
            ..SimConfig::default()
        };
        if let Some(addr) = args.phone_address {
            cfg.phone_address = addr;
        }
        install_signal_handlers();
        let sim = match Sim::start(cfg, modem) {
            Ok(s) => s,
            Err(e @ SimError::NameTaken) => {
                eprintln!("geekbridge-phone-sim: {e}");
                return 3;
            }
            Err(e) => {
                eprintln!("geekbridge-phone-sim: cannot start: {e}");
                return 1;
            }
        };
        println!(
            "geekbridge-phone-sim: serving org.pipewire.Telephony as {} on {} (modem: {label}); simulated signalling only",
            sim.unique_name(),
            args.bus.as_deref().unwrap_or("the session bus")
        );
        sim.run_until(&STOP);
        eprintln!("geekbridge-phone-sim: stopping");
        0
    }
}
