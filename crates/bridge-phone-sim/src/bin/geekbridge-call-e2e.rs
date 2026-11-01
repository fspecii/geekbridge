//! End-to-end call harness, desktop side.
//!
//! Drives the real `bridge-calls` stack (`Session` over `PipeWireBackend`) against a simulator
//! service that serves `org.pipewire.Telephony`, while a `Modem + RemoteParty` pair plays the
//! phone and the remote caller. Only call *signalling* is simulated: no Bluetooth link, SCO audio
//! or AT command exchange takes place, so a PASS here says nothing about audio or hardware.
//!
//! Exit codes: 0 all selected scenarios passed, 1 a scenario failed, 2 usage/environment error.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "geekbridge-call-e2e: unsupported platform: the end-to-end harness drives the Linux \
         PipeWire org.pipewire.Telephony D-Bus stack and needs Linux"
    );
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[path = "../../tests/support/scenarios.rs"]
mod scenarios;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    app::main()
}

#[cfg(target_os = "linux")]
mod app {
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::time::Duration;

    use bridge_calls::pipewire::BusTarget;
    use bridge_phone_sim::{AdbModem, FakeModem, Modem};

    use crate::scenarios::{
        Caps, DEFAULT_NUMBER, DEFAULT_STEP_TIMEOUT, EMULATOR_NUMBER, Harness, Options, Phone,
        SelfContained, Status, render_json, run_selected, scenario_ids,
    };

    const USAGE: &str = "\
geekbridge-call-e2e: desktop-side end-to-end call scenarios (simulated signalling only)

USAGE:
  geekbridge-call-e2e --fake [options]
  geekbridge-call-e2e --adb PATH --serial SERIAL [--adb-port N] [--bus ADDR | --spawn-sim] [options]

MODES (exactly one):
  --fake               self-contained: spawn a private dbus-daemon and an in-process simulator
                       over an in-memory FakeModem (the only mode that can run S7)
  --adb PATH           use the Android emulator through this adb binary as the phone; needs
                       --serial. Connects to an already running simulator service unless
                       --spawn-sim is given

OPTIONS:
  --serial S           emulator serial (for example emulator-5554)
  --adb-port N         adb server port
  --bus ADDR           D-Bus address of the simulator's bus (default: the session bus from
                       DBUS_SESSION_BUS_ADDRESS)
  --spawn-sim          with --adb: also start a private dbus-daemon and an in-process simulator
                       over the emulator, instead of connecting to a running one
  --only S1,S4         run only these scenarios (S1..S8)
  --timeout SECS       per-step deadline in seconds (default 15; fractions allowed)
  --interval-ms N      with --spawn-sim: simulator modem poll interval (default 300, the real
                       binary's default; --fake always uses 25)
  --number DIGITS      fictional number to use (default 5551234, or 5557701 with --adb so it
                       does not collide with other tools using the emulator); only its last two
                       digits are ever printed
  --json               print one JSON document instead of per-step lines
  -h, --help           this text

With --adb the scenarios adapt to the emulator's measured modem (`docs/architecture.md`):
it auto-answers calls the desktop places after about 2 s and cannot be accepted or hung up from the
console, so S4 ends with a desktop Hangup and S5 hangs up inside the dialing window (retrying up to
twice if the call is answered first, reported in the output).

Scenarios: S1 incoming answered, S2 incoming rejected, S3 missed, S4 outgoing, S5 outgoing
cancelled, S6 guard rails, S7 link loss (--fake or --spawn-sim only), S8 service call hub
(decline, answer and hang up through the view the Calls window uses).

EXIT: 0 all passed, 1 a step failed, 2 usage or environment error";

    struct Args {
        fake: bool,
        adb: Option<PathBuf>,
        serial: Option<String>,
        adb_port: Option<u16>,
        bus: Option<String>,
        spawn_sim: bool,
        only: Vec<String>,
        timeout: Duration,
        number: Option<String>,
        interval: Duration,
        interval_given: bool,
        json: bool,
    }

    enum Parsed {
        Run(Box<Args>),
        Help,
    }

    fn parse_args(raw: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
        let mut a = Args {
            fake: false,
            adb: None,
            serial: None,
            adb_port: None,
            bus: None,
            spawn_sim: false,
            only: Vec::new(),
            timeout: DEFAULT_STEP_TIMEOUT,
            number: None,
            interval: Duration::from_millis(300),
            interval_given: false,
            json: false,
        };
        let mut it = raw.into_iter();
        while let Some(arg) = it.next() {
            let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
            match arg.as_str() {
                "-h" | "--help" => return Ok(Parsed::Help),
                "--fake" => a.fake = true,
                "--spawn-sim" => a.spawn_sim = true,
                "--json" => a.json = true,
                "--adb" => a.adb = Some(PathBuf::from(value("--adb")?)),
                "--serial" => a.serial = Some(value("--serial")?),
                "--bus" => a.bus = Some(value("--bus")?),
                "--number" => a.number = Some(value("--number")?),
                "--interval-ms" => {
                    let ms: u64 = value("--interval-ms")?.parse().map_err(|_| {
                        "--interval-ms must be a number of milliseconds".to_string()
                    })?;
                    if !(10..=60_000).contains(&ms) {
                        return Err("--interval-ms must be between 10 and 60000".to_string());
                    }
                    a.interval = Duration::from_millis(ms);
                    a.interval_given = true;
                }
                "--adb-port" => {
                    a.adb_port = Some(
                        value("--adb-port")?
                            .parse()
                            .map_err(|_| "--adb-port must be a port number".to_string())?,
                    )
                }
                "--timeout" => {
                    let secs: f64 = value("--timeout")?
                        .parse()
                        .map_err(|_| "--timeout must be a number of seconds".to_string())?;
                    if !secs.is_finite() || secs <= 0.0 || secs > 3600.0 {
                        return Err("--timeout must be between 0 and 3600 seconds".to_string());
                    }
                    a.timeout = Duration::from_secs_f64(secs);
                }
                "--only" => {
                    for id in value("--only")?.split(',').filter(|s| !s.is_empty()) {
                        let id = id.trim().to_ascii_uppercase();
                        if !scenario_ids().contains(&id.as_str()) {
                            return Err(format!(
                                "unknown scenario {id:?}; known: {}",
                                scenario_ids().join(",")
                            ));
                        }
                        a.only.push(id);
                    }
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        if a.fake == a.adb.is_some() {
            return Err("choose exactly one of --fake or --adb PATH".to_string());
        }
        if a.fake && (a.serial.is_some() || a.adb_port.is_some() || a.spawn_sim) {
            return Err("--serial, --adb-port and --spawn-sim apply to --adb only".to_string());
        }
        if a.adb.is_some() && a.serial.is_none() {
            return Err("--adb needs --serial".to_string());
        }
        if a.spawn_sim && a.bus.is_some() {
            return Err("--spawn-sim and --bus are mutually exclusive".to_string());
        }
        if a.fake && a.bus.is_some() {
            return Err("--fake creates its own bus; do not pass --bus".to_string());
        }
        if a.interval_given && !a.spawn_sim {
            return Err("--interval-ms applies to --spawn-sim only".to_string());
        }
        if let Some(n) = &a.number
            && (!n.chars().all(|c| c.is_ascii_digit() || c == '+') || n.len() < 4)
        {
            return Err("--number must be at least 4 digits (optionally +-prefixed)".to_string());
        }
        let can_stop_service = a.fake || a.spawn_sim;
        if !can_stop_service && a.only.iter().any(|id| id == "S7") {
            return Err(
                "S7 needs --fake or --spawn-sim (the harness must own the simulator)".into(),
            );
        }
        Ok(Parsed::Run(Box::new(a)))
    }

    fn env_error(msg: impl std::fmt::Display) -> ExitCode {
        eprintln!("error: {msg}");
        ExitCode::from(2)
    }

    pub fn main() -> ExitCode {
        let args = match parse_args(std::env::args().skip(1)) {
            Ok(Parsed::Help) => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Ok(Parsed::Run(a)) => *a,
            Err(e) => {
                eprintln!("error: {e}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };

        let opts = Options {
            step_timeout: args.timeout,
            number: args.number.clone().unwrap_or_else(|| {
                if args.fake {
                    DEFAULT_NUMBER
                } else {
                    EMULATOR_NUMBER
                }
                .to_string()
            }),
            caps: if args.fake {
                Caps::FAKE
            } else {
                Caps::EMULATOR
            },
        };

        // Keep the self-contained environment alive until the end of main.
        let mut owned: Option<SelfContained> = None;
        let mut harness = if args.fake {
            let modem = FakeModem::new();
            let sim_side: Arc<dyn Modem> = Arc::new(modem.clone());
            let phone_side: Arc<dyn Phone> = Arc::new(modem);
            match SelfContained::start(sim_side, phone_side) {
                Ok(env) => owned.insert(env).try_harness(opts),
                Err(e) => return env_error(e),
            }
        } else {
            let adb = Arc::new(AdbModem::new(
                args.adb.clone().unwrap_or_default(),
                args.serial.clone().unwrap_or_default(),
                args.adb_port,
            ));
            if args.spawn_sim {
                let sim_side: Arc<dyn Modem> = adb.clone();
                let phone_side: Arc<dyn Phone> = adb;
                match SelfContained::start_with_interval(sim_side, phone_side, args.interval) {
                    Ok(env) => owned.insert(env).try_harness(opts),
                    Err(e) => return env_error(e),
                }
            } else {
                let bus = match &args.bus {
                    Some(addr) => BusTarget::Address(addr.clone()),
                    None if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() => {
                        BusTarget::Session
                    }
                    None => {
                        return env_error(
                            "no --bus given and DBUS_SESSION_BUS_ADDRESS is not set; cannot find the simulator's bus",
                        );
                    }
                };
                Harness::connect(bus, adb, opts)
            }
        }
        .unwrap_or_else(|e| {
            eprintln!("error: cannot connect the desktop session: {e}");
            std::process::exit(2);
        });

        if !args.json {
            eprintln!(
                "note: simulated call signalling only; no Bluetooth, SCO audio or AT exchange is exercised"
            );
            harness.set_sink(|rec| {
                println!("{}", rec.line());
                let _ = std::io::stdout().flush();
            });
        }

        let reports = run_selected(&mut harness, &args.only);
        if args.json {
            println!("{}", render_json(&reports));
        } else {
            println!();
            for r in &reports {
                println!("{}", r.summary_line());
            }
        }
        drop(harness);
        drop(owned);
        if reports.iter().any(|r| r.status == Status::Fail) {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        }
    }
}
