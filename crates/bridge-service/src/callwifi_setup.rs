// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-ctl callwifi setup`: the one-time, guided start of the phone's call-audio helper for the EXPERIMENTAL
//! "Call audio over Wi-Fi" (`docs/design/call-audio-over-wifi.md`; Bluetooth stays the supported path).
//!
//! It walks through Wireless debugging pairing with `adb`, turns this computer's consent on for the phone (asked),
//! checks the phone runs a GeekBridge DEBUG build, asks the app for its one-time loopback port and token, and starts
//! the helper with `app_process` as the shell user, exactly like `scripts/callaudio-enable.sh`. It then waits for the
//! phone to report the helper ready and says what to do next. Nothing is installed or written on the phone.
//!
//! Every `adb` call goes through [`Runner`] and every question through [`Io`], so the whole walk is tested with a
//! scripted stand-in phone; nothing here ever runs against a device by itself.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use bridge_core::call_wifi_view::{CallWifiAction, CallWifiView};
use bridge_core::ipc::{self, Feature, Request, Response, Token};

pub const PKG: &str = "dev.geekbridge.android";
pub const HELPER: &str = "dev.geekbridge.android.callaudio.CallAudioHelper";
const RECEIVER: &str = "dev.geekbridge.android/.callaudio.CallWifiReceiver";

pub const USAGE: &str = "usage: geekbridge-ctl callwifi setup [--serial SERIAL] [--pair HOST:PORT CODE] \
[--connect HOST:PORT] [--adb PATH] [--yes] | setup --status | setup --stop";

/// Runs `adb` with these arguments: (exit code, stdout and stderr together, `\r` removed).
pub trait Runner {
    fn adb(&mut self, args: &[&str]) -> (i32, String);
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// The person at the keyboard.
pub trait Io {
    fn say(&mut self, line: &str);
    /// One answer line, trimmed; empty at end of input.
    fn ask(&mut self, prompt: &str) -> String;
}

/// The service, when it runs.
pub trait Service {
    fn view(&mut self) -> Option<CallWifiView>;
    fn allow(&mut self, device_id: &str) -> Result<(), String>;
}

// ---------------------------------------------------------------------------------------------- pure helpers

/// `adb devices` → serials whose state is `device`.
pub fn parse_devices(out: &str) -> Vec<String> {
    out.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            match (w.next(), w.next()) {
                (Some(s), Some("device")) => Some(s.to_owned()),
                _ => None,
            }
        })
        .collect()
}

/// `adb mdns services` → (pairing addresses, connect addresses), `HOST:PORT` each.
pub fn parse_mdns(out: &str) -> (Vec<String>, Vec<String>) {
    let (mut pair, mut conn) = (Vec::new(), Vec::new());
    for l in out.lines() {
        let Some(addr) = l.split_whitespace().last().filter(|a| valid_hostport(a)) else {
            continue;
        };
        if l.contains("_adb-tls-pairing._tcp") {
            pair.push(addr.to_owned());
        } else if l.contains("_adb-tls-connect._tcp") {
            conn.push(addr.to_owned());
        }
    }
    (pair, conn)
}

pub fn valid_hostport(s: &str) -> bool {
    let Some((h, p)) = s.rsplit_once(':') else {
        return false;
    };
    !h.is_empty()
        && h.len() <= 253
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']'))
        && p.parse::<u16>().is_ok_and(|p| p > 0)
}

pub fn valid_code(s: &str) -> bool {
    s.len() == 6 && s.chars().all(|c| c.is_ascii_digit())
}

/// The app's answer to the OPEN broadcast.
#[derive(Debug, PartialEq, Eq)]
pub enum Open {
    Listening { port: u16, token: String },
    SwitchOff,
    NoAnswer,
}

pub fn parse_open(out: &str) -> Open {
    if out.contains("result=2") {
        return Open::SwitchOff;
    }
    if !out.contains("result=1") {
        return Open::NoAnswer;
    }
    let field = |k: &str| {
        out.split(|c: char| c.is_whitespace() || c == '"')
            .find_map(|w| w.strip_prefix(k))
            .map(str::to_owned)
    };
    match (
        field("port=").and_then(|p| p.parse::<u16>().ok()),
        field("token=").filter(|t| {
            (8..=64).contains(&t.len())
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }),
    ) {
        (Some(port), Some(token)) if port >= 1024 => Open::Listening { port, token },
        _ => Open::NoAnswer,
    }
}

/// `ps -A -o PID,ARGS` → pids of the helper.
pub fn helper_pids(ps: &str) -> Vec<u32> {
    ps.lines()
        .filter(|l| l.contains(HELPER))
        .filter_map(|l| l.split_whitespace().next()?.parse().ok())
        .collect()
}

/// Where `adb` is: `--adb`, `$ADB`, `PATH`, the Android SDK, the repository's `.tools`.
pub fn find_adb(explicit: Option<&str>) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut c: Vec<PathBuf> = Vec::new();
    c.extend(explicit.map(PathBuf::from));
    c.extend(env("ADB"));
    if let Some(path) = std::env::var_os("PATH") {
        c.extend(std::env::split_paths(&path).map(|d| d.join("adb")));
    }
    for k in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        c.extend(env(k).map(|d| d.join("platform-tools/adb")));
    }
    c.extend(env("HOME").map(|h| h.join("Android/Sdk/platform-tools/adb")));
    // A build from the repository: target/<profile>/geekbridge-ctl → <repo>/.tools/android-sdk/platform-tools/adb.
    const REL: &str = ".tools/android-sdk/platform-tools/adb";
    if let Some(root) = exe.as_deref().and_then(|e| e.ancestors().nth(3)) {
        c.push(root.join(REL));
    }
    // Also next to the executable's parents (a copy elsewhere) and the current directory and its parents.
    if let Some(dir) = exe.as_deref().and_then(Path::parent) {
        c.extend(dir.ancestors().take(5).map(|d| d.join(REL)));
    }
    if let Ok(cwd) = std::env::current_dir() {
        c.extend(cwd.ancestors().take(5).map(|d| d.join(REL)));
    }
    c.into_iter().find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

// ---------------------------------------------------------------------------------------------- the walk

#[derive(Debug, Default, Clone)]
pub struct Opts {
    pub serial: Option<String>,
    pub pair: Option<(String, String)>,
    pub connect: Option<String>,
    pub adb: Option<String>,
    pub yes: bool,
    pub status: bool,
    pub stop: bool,
}

pub fn parse_opts(words: &[&str]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    let val = |i: usize| words.get(i).map(|s| (*s).to_owned()).ok_or(USAGE);
    while i < words.len() {
        match words[i] {
            "--serial" => o.serial = Some(val(i + 1)?),
            "--adb" => o.adb = Some(val(i + 1)?),
            "--connect" => o.connect = Some(val(i + 1)?),
            "--pair" => {
                o.pair = Some((val(i + 1)?, val(i + 2)?));
                i += 1;
            }
            "--yes" => {
                o.yes = true;
                i += 1;
                continue;
            }
            "--status" => {
                o.status = true;
                i += 1;
                continue;
            }
            "--stop" => {
                o.stop = true;
                i += 1;
                continue;
            }
            _ => return Err(USAGE.into()),
        }
        i += 2;
    }
    if let Some((hp, code)) = &o.pair {
        if !valid_hostport(hp) {
            return Err("--pair wants HOST:PORT from the phone's pairing dialog".into());
        }
        if !valid_code(code) {
            return Err("the pairing code is the 6 digits shown on the phone".into());
        }
    }
    if o.connect.as_deref().is_some_and(|c| !valid_hostport(c)) {
        return Err("--connect wants HOST:PORT from the Wireless debugging screen".into());
    }
    Ok(o)
}

const INTRO: &str = "\
Call audio over Wi-Fi (EXPERIMENTAL; Bluetooth stays the supported way)
This starts a small helper on your phone with the powers of Android's \"shell\" user (what `adb shell` can do).
It lets GeekBridge hear the OTHER person in a phone call and, if your phone allows it, put this computer's
microphone into the call. Only while you press Listen or Speak, with every switch on. Nothing is recorded.
Sharing or recording calls may be illegal where you live without the other person's consent.";

struct Walk<'a> {
    run: &'a mut dyn Runner,
    io: &'a mut dyn Io,
    serial: Option<String>,
}

impl Walk<'_> {
    fn adb(&mut self, args: &[&str]) -> (i32, String) {
        match &self.serial {
            Some(s) => {
                let mut a = vec!["-s", s.as_str()];
                a.extend_from_slice(args);
                let s = a.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
                let refs: Vec<&str> = s.iter().map(String::as_str).collect();
                self.run.adb(&refs)
            }
            None => self.run.adb(args),
        }
    }

    fn shell(&mut self, cmd: &str) -> String {
        self.adb(&["shell", cmd]).1
    }

    fn pids(&mut self) -> Vec<u32> {
        let ps = self.shell("ps -A -o PID,ARGS");
        helper_pids(&ps)
    }

    fn yes(&mut self, prompt: &str, assume: bool) -> bool {
        if assume {
            return true;
        }
        matches!(
            self.io.ask(&format!("{prompt} [y/N] ")).as_str(),
            "y" | "Y" | "yes" | "YES"
        )
    }

    /// Pick the phone: the one given, the only one connected, or pair and connect over Wi-Fi now.
    /// `pairing` false (status/stop): never start the interactive pairing walk.
    fn choose_phone(&mut self, o: &Opts, pairing: bool) -> Result<(), String> {
        if let Some((hp, code)) = &o.pair {
            self.pair(hp, code)?;
        }
        if let Some(c) = &o.connect {
            self.connect(c)?;
            self.serial = Some(c.clone());
        }
        if self.serial.is_some() {
            return Ok(());
        }
        for round in 0..2 {
            let devs = parse_devices(&self.run.adb(&["devices"]).1);
            match devs.as_slice() {
                [one] => {
                    self.io.say(&format!("Using the phone {one}."));
                    self.serial = Some(one.clone());
                    return Ok(());
                }
                [] if !pairing => {
                    return Err("phone not reachable over adb (no pairing is started for --status/--stop). \
                         Turn Wireless debugging on and connect it, or use the phone's own switch."
                        .into());
                }
                [] if round == 0 => self.pair_interactively()?,
                [] => break,
                many => {
                    self.io.say("Several phones are connected:");
                    for (i, d) in many.iter().enumerate() {
                        self.io.say(&format!("  {}. {d}", i + 1));
                    }
                    let n = self.io.ask("Which one (number)? ");
                    let pick = n
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| many.get(n.wrapping_sub(1)))
                        .ok_or("no phone chosen (or use --serial)")?;
                    self.serial = Some(pick.clone());
                    return Ok(());
                }
            }
        }
        Err("no phone is connected over adb. Check that Wireless debugging is on and run this again.".into())
    }

    fn pair(&mut self, hp: &str, code: &str) -> Result<(), String> {
        let (rc, out) = self.run.adb(&["pair", hp, code]);
        if rc != 0 || !out.to_lowercase().contains("success") {
            return Err(format!(
                "pairing failed ({}). Keep the pairing dialog open on the phone and use a fresh code.",
                out.lines().last().unwrap_or("").trim()
            ));
        }
        self.io.say("Paired.");
        Ok(())
    }

    fn connect(&mut self, hp: &str) -> Result<(), String> {
        let (_, out) = self.run.adb(&["connect", hp]);
        if !out.contains("connected to") {
            return Err(format!(
                "could not connect to {hp} ({}). The port on the Wireless debugging screen changes each time it is turned on.",
                out.lines().last().unwrap_or("").trim()
            ));
        }
        Ok(())
    }

    fn pair_interactively(&mut self) -> Result<(), String> {
        self.io.say(
            "\nNo phone is connected over adb yet. On the phone:\n\
             \x20 1. Settings > About phone > Software information: tap \"Build number\" 7 times (Developer options).\n\
             \x20 2. Settings > Developer options > Wireless debugging: switch it on (same Wi-Fi as this computer).\n\
             \x20 3. Tap \"Pair device with pairing code\" and leave that dialog open.",
        );
        let (pairing, _) = parse_mdns(&self.run.adb(&["mdns", "services"]).1);
        let hint = pairing.first().cloned();
        let hp = match &hint {
            Some(h) => {
                let a = self
                    .io
                    .ask(&format!("Pairing address shown on the phone [{h}]: "));
                if a.is_empty() { h.clone() } else { a }
            }
            None => self.io.ask(
                "Pairing address shown on the phone (IP address & port, e.g. 192.168.1.20:37123): ",
            ),
        };
        if !valid_hostport(&hp) {
            return Err("that is not an address like 192.168.1.20:37123".into());
        }
        let code = self.io.ask("Wi-Fi pairing code (6 digits): ");
        if !valid_code(&code) {
            return Err("the pairing code is the 6 digits shown on the phone".into());
        }
        self.pair(&hp, &code)?;
        // adb usually connects by itself after pairing (it finds the phone on the network); give it a moment.
        for _ in 0..6 {
            if !parse_devices(&self.run.adb(&["devices"]).1).is_empty() {
                return Ok(());
            }
            let (_, conn) = parse_mdns(&self.run.adb(&["mdns", "services"]).1);
            if let [one] = conn.as_slice() {
                let one = one.clone();
                if self.connect(&one).is_ok() {
                    return Ok(());
                }
            }
            self.run.sleep(Duration::from_secs(1));
        }
        let hp = self.io.ask(
            "Now the address on the Wireless debugging screen itself (IP address & Port, not the pairing one): ",
        );
        if !valid_hostport(&hp) {
            return Err("that is not an address like 192.168.1.20:41234".into());
        }
        self.connect(&hp)
    }

    fn check_phone(&mut self) -> Result<(), String> {
        let state = self.adb(&["get-state"]).1;
        if state.trim() != "device" {
            return Err(format!(
                "the phone is not ready (adb says: {}). Unlock it and accept the debugging prompt.",
                state.trim()
            ));
        }
        if !self.shell(&format!("pm path {PKG}")).contains("package:") {
            return Err("GeekBridge is not installed on this phone".into());
        }
        let dbg = self.shell(&format!("run-as {PKG} id -u"));
        if dbg.contains("not debuggable")
            || dbg.contains("Unknown package")
            || dbg.contains("package not found")
        {
            return Err(
                "this GeekBridge is a release build; Wi-Fi call audio exists only in debug builds"
                    .into(),
            );
        }
        Ok(())
    }

    fn app_uid(&mut self) -> Result<String, String> {
        let out = self.shell(&format!("pm list packages -U {PKG}"));
        out.split_whitespace()
            .find_map(|w| w.strip_prefix("uid:"))
            .map(|u| u.split(',').next().unwrap_or(u).to_owned())
            .filter(|u| !u.is_empty() && u.chars().all(|c| c.is_ascii_digit()))
            .ok_or_else(|| "could not read the app's uid".into())
    }

    fn open(&mut self, assume_yes: bool) -> Result<(u16, String), String> {
        for _ in 0..3 {
            match parse_open(&self.shell(&format!(
                "am broadcast -n {RECEIVER} -a dev.geekbridge.callwifi.OPEN"
            ))) {
                Open::Listening { port, token } => return Ok((port, token)),
                Open::SwitchOff => {
                    self.io.say(
                        "The phone's switch is off. On the phone: GeekBridge > Settings > Experimental > \
                         Call audio over Wi-Fi (ADB helper): switch it on and confirm.",
                    );
                    if assume_yes {
                        self.run.sleep(Duration::from_secs(5));
                    } else {
                        self.io.ask("Press Enter when it is on... ");
                    }
                }
                Open::NoAnswer => {
                    return Err(
                        "the GeekBridge app did not answer. Is it the debug build, and up to date?"
                            .into(),
                    );
                }
            }
        }
        Err("the switch on the phone is still off".into())
    }

    fn start_helper(&mut self, port: u16, token: &str, uid: &str) -> Result<(), String> {
        for p in self.pids() {
            self.shell(&format!("kill {p}"));
        }
        let cmd = format!(
            "CLASSPATH=$(pm path {PKG} | sed s/^package://) setsid nohup app_process /system/bin {HELPER} \
             --connect {port} --token {token} --app-uid {uid} >/dev/null 2>&1 &"
        );
        self.adb(&["shell", &cmd]);
        for _ in 0..10 {
            self.run.sleep(Duration::from_millis(500));
            if !self.pids().is_empty() {
                return Ok(());
            }
        }
        Err("the helper did not start (reinstall the current debug build and try again)".into())
    }
}

/// The walk. `svc` is `None` when the service is not running (the consent step is then skipped, with a note).
pub fn setup(
    o: &Opts,
    run: &mut dyn Runner,
    io: &mut dyn Io,
    svc: Option<&mut dyn Service>,
) -> Result<(), String> {
    let mut w = Walk {
        run,
        io,
        serial: o.serial.clone(),
    };
    if o.status || o.stop {
        w.choose_phone(
            &Opts {
                pair: None,
                connect: None,
                ..o.clone()
            },
            false,
        )?;
        if o.stop {
            for p in w.pids() {
                w.shell(&format!("kill {p}"));
            }
            w.shell(&format!(
                "am broadcast -n {RECEIVER} -a dev.geekbridge.callwifi.STOP"
            ));
            w.run.sleep(Duration::from_secs(1));
            if !w.pids().is_empty() {
                return Err("the helper is still running; restart the phone to end it".into());
            }
            w.io.say("Helper stopped. Turn Wireless debugging off on the phone if you no longer need it.");
        } else {
            let p = w.pids();
            w.io.say(&if p.is_empty() {
                "helper not running".to_owned()
            } else {
                format!("helper running (pid {p:?})")
            });
        }
        return Ok(());
    }

    w.io.say(INTRO);
    if !w.yes("\nContinue?", o.yes) {
        w.io.say("Nothing was changed.");
        return Ok(());
    }

    // 1. This computer: is a phone offering it, and is it allowed here?
    let mut svc = svc;
    let mut phone_id = None;
    match svc.as_deref_mut().and_then(|s| s.view()) {
        None => w.io.say(
            "\n[1/4] The GeekBridge service is not running here; start it first for Listen/Speak \
             (the helper can still be started).",
        ),
        Some(v) if !v.supported => w.io.say(&format!(
            "\n[1/4] This computer cannot play call audio: {}",
            v.problem.as_deref().unwrap_or("unsupported")
        )),
        Some(v) => match v.phones.as_slice() {
            [] => w.io.say(
                "\n[1/4] No connected phone offers Wi-Fi call audio yet. On the phone (debug build): \
                 GeekBridge > Settings > Experimental > Call audio over Wi-Fi: switch it on, and allow this computer.",
            ),
            phones => {
                let p = &phones[0];
                phone_id = Some(p.device_id.clone());
                if p.allowed {
                    w.io.say(&format!("\n[1/4] {} is allowed on this computer.", p.name));
                } else if w.yes(
                    &format!("\n[1/4] Allow Wi-Fi call audio from {} on this computer?", p.name),
                    o.yes,
                ) {
                    if let Some(s) = svc.as_deref_mut() {
                        s.allow(&p.device_id)?;
                    }
                    w.io.say("Allowed (turn off: geekbridge-ctl consent ID call_audio_wifi off).");
                }
            }
        },
    }

    // 2. adb and the phone.
    w.io.say("\n[2/4] Connecting to the phone with adb (Wireless debugging)...");
    w.choose_phone(o, true)?;
    w.check_phone()?;
    let uid = w.app_uid()?;

    // 3. The helper.
    w.io.say("[3/4] Starting the helper on the phone...");
    let (port, token) = w.open(o.yes)?;
    w.start_helper(port, &token, &uid)?;
    w.io.say("Helper started; it connects to the GeekBridge app on the phone.");

    // 4. Wait for the phone to say so.
    if let (Some(s), Some(id)) = (svc, phone_id.as_deref()) {
        let mut said = None;
        for _ in 0..20 {
            if let Some(p) = s
                .view()
                .and_then(|v| v.phones.into_iter().find(|p| p.device_id == id))
                .filter(|p| p.ready)
            {
                said = Some(p.test);
                break;
            }
            w.run.sleep(Duration::from_secs(1));
        }
        match said {
            Some(t) => w.io.say(&format!("[4/4] The phone reports its helper ready (self-test: {t}).")),
            None => w.io.say(
                "[4/4] The phone has not reported the helper yet (is it allowed on the phone for this computer?).",
            ),
        }
    }
    w.io.say(NEXT);
    Ok(())
}

const NEXT: &str = "
Next:
  1. On the phone, Settings > Experimental > Call audio over Wi-Fi > Self-test:
       step 1 with NO call (must end \"quiet\"), step 2 during a call, step 3 \"Test speaking\" during a call
       (the other person should hear beeps; tell the phone how many).
  2. Headphones and microphone on this computer: geekbridge-ctl call-audio use --output NAME --input NAME
       (a headset is strongly recommended; with speakers the echo guard closes your microphone while the
       other person talks: geekbridge-ctl callwifi echo auto|on|off).
  3. During a call: geekbridge-ctl callwifi listen, then geekbridge-ctl callwifi speak on (or the buttons on the
       call card). Numbers: geekbridge-ctl callwifi status.
  4. Done: geekbridge-ctl callwifi setup --stop, and turn Wireless debugging off.";

// ---------------------------------------------------------------------------------------------- the real ones

pub struct AdbRunner(pub PathBuf);

impl Runner for AdbRunner {
    fn adb(&mut self, args: &[&str]) -> (i32, String) {
        match Command::new(&self.0)
            .args(args)
            .stdin(Stdio::null())
            .output()
        {
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                s.push_str(&String::from_utf8_lossy(&o.stderr));
                (o.status.code().unwrap_or(-1), s.replace('\r', ""))
            }
            Err(e) => (-1, e.to_string()),
        }
    }
}

pub struct Terminal;

impl Io for Terminal {
    fn say(&mut self, line: &str) {
        println!("{line}");
    }
    fn ask(&mut self, prompt: &str) -> String {
        use std::io::Write as _;
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
        s.trim().to_owned()
    }
}

pub struct Ipc {
    pub port: u16,
    pub token: Token,
}

impl Service for Ipc {
    fn view(&mut self) -> Option<CallWifiView> {
        match ipc::call(
            self.port,
            &self.token,
            &Request::CallWifi {
                action: CallWifiAction::Status,
            },
        ) {
            Ok(Response::CallWifi(v)) => Some(*v),
            _ => None,
        }
    }
    fn allow(&mut self, device_id: &str) -> Result<(), String> {
        match ipc::call(
            self.port,
            &self.token,
            &Request::SetConsent {
                device_id: device_id.to_owned(),
                feature: Feature::CallAudioWifi,
                enabled: true,
            },
        )
        .map_err(|e| e.to_string())?
        {
            Response::Error(e) => Err(e.message),
            _ => Ok(()),
        }
    }
}

/// `geekbridge-ctl callwifi setup ...` (`words` after `setup`).
pub fn run(words: &[&str], port: u16, token: Option<Token>) -> Result<(), String> {
    let o = parse_opts(words)?;
    let adb = find_adb(o.adb.as_deref()).ok_or(
        "adb not found. Install Android platform-tools (Arch: pacman -S android-tools; Debian/Ubuntu: apt install adb; \
         Fedora: dnf install android-tools) or pass --adb PATH.",
    )?;
    let mut svc = token.map(|token| Ipc { port, token });
    setup(
        &o,
        &mut AdbRunner(adb),
        &mut Terminal,
        svc.as_mut().map(|s| s as &mut dyn Service),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::call_wifi_view::CallWifiPhone;
    use std::collections::VecDeque;

    #[test]
    fn parsers_read_what_adb_and_the_app_print() {
        assert_eq!(
            parse_devices(
                "* daemon started\nList of devices attached\nR5CW1 device\n192.168.1.5:4111\toffline\nemulator-5554\tdevice\n\n"
            ),
            ["R5CW1", "emulator-5554"]
        );
        let (p, c) = parse_mdns(
            "List of discovered mdns services\nadb-R5CW-Ab\t_adb-tls-pairing._tcp\t192.168.1.20:37123\n\
             adb-R5CW-Ab\t_adb-tls-connect._tcp\t192.168.1.20:41234\nbogus\t_adb-tls-connect._tcp\tnot-an-address\n",
        );
        assert_eq!(
            (p, c),
            (
                vec!["192.168.1.20:37123".to_owned()],
                vec!["192.168.1.20:41234".to_owned()]
            )
        );
        assert_eq!(
            parse_open(
                "Broadcasting: Intent\nBroadcast completed: result=1, data=\"port=40123 token=t0123456789abcdef\""
            ),
            Open::Listening {
                port: 40123,
                token: "t0123456789abcdef".into()
            }
        );
        assert_eq!(parse_open("Broadcast completed: result=2"), Open::SwitchOff);
        assert_eq!(parse_open("Broadcast completed: result=0"), Open::NoAnswer);
        assert_eq!(
            parse_open("result=1, data=\"port=80 token=t0123456789\""),
            Open::NoAnswer
        );
        assert_eq!(
            parse_open("result=1, data=\"port=40000 token=a;rm -rf\""),
            Open::NoAnswer
        );
        assert_eq!(
            helper_pids(&format!(
                "  PID ARGS\n 4242 app_process /system/bin {HELPER} --connect 1\n 99 sh\n"
            )),
            [4242]
        );
        assert!(valid_hostport("192.168.1.20:37123") && valid_hostport("[fe80::1]:5555"));
        assert!(
            !valid_hostport("192.168.1.20") && !valid_hostport("a b:12") && !valid_hostport("h:0")
        );
        assert!(valid_code("123456") && !valid_code("12345") && !valid_code("12345a"));
        assert!(parse_opts(&["--pair", "1.2.3.4:5", "12345"]).is_err());
        assert!(parse_opts(&["--connect", "x"]).is_err());
        let o = parse_opts(&["--pair", "1.2.3.4:5", "123456", "--yes", "--serial", "S"]).unwrap();
        assert!(o.yes && o.serial.as_deref() == Some("S") && o.pair.is_some());
        assert!(parse_opts(&["--what"]).is_err());
    }

    /// A stand-in phone behind `adb`: answers like a debug build whose switch is on, records every command.
    #[derive(Default)]
    struct FakePhone {
        log: Vec<String>,
        devices: Vec<&'static str>,
        paired: bool,
        switch_on: bool,
        release_build: bool,
        helper: bool,
    }

    impl Runner for FakePhone {
        fn adb(&mut self, args: &[&str]) -> (i32, String) {
            let line = args.join(" ");
            self.log.push(line.clone());
            let a: Vec<&str> = if args.first() == Some(&"-s") {
                args[2..].to_vec()
            } else {
                args.to_vec()
            };
            let shell =
                |c: &str| a.first() == Some(&"shell") && a.get(1).is_some_and(|x| x.starts_with(c));
            let out = match a.first().copied() {
                Some("devices") => {
                    let mut s = String::from("List of devices attached\n");
                    for d in &self.devices {
                        s.push_str(&format!("{d}\tdevice\n"));
                    }
                    s
                }
                Some("mdns") => "adb-X\t_adb-tls-pairing._tcp\t192.168.1.20:37123\n".into(),
                Some("pair") => {
                    self.paired = true;
                    self.devices.push("192.168.1.20:41234");
                    "Successfully paired to 192.168.1.20:37123".into()
                }
                Some("connect") => "connected to x".into(),
                Some("get-state") => "device\n".into(),
                _ if shell("pm path") => "package:/data/app/x/base.apk\n".into(),
                _ if shell("run-as") => {
                    if self.release_build {
                        "run-as: package not debuggable: dev.geekbridge.android".into()
                    } else {
                        "10234\n".into()
                    }
                }
                _ if shell("pm list packages -U") => {
                    "package:dev.geekbridge.android uid:10234\n".into()
                }
                _ if shell("am broadcast") && line.contains("OPEN") => {
                    if self.switch_on {
                        "Broadcast completed: result=1, data=\"port=40123 token=t00112233445566778899aabbccddeeff\"".into()
                    } else {
                        self.switch_on = true; // the person switches it on when asked
                        "Broadcast completed: result=2".into()
                    }
                }
                _ if shell("ps") => {
                    if self.helper {
                        format!("1 init\n4242 app_process /system/bin {HELPER} --connect 40123\n")
                    } else {
                        "1 init\n".into()
                    }
                }
                _ if shell("CLASSPATH=") => {
                    self.helper = true;
                    String::new()
                }
                _ if shell("kill") => {
                    self.helper = false;
                    String::new()
                }
                _ => String::new(),
            };
            (0, out)
        }
        fn sleep(&mut self, _: Duration) {}
    }

    #[derive(Default)]
    struct Script {
        answers: VecDeque<&'static str>,
        said: Vec<String>,
    }
    impl Io for Script {
        fn say(&mut self, line: &str) {
            self.said.push(line.to_owned());
        }
        fn ask(&mut self, prompt: &str) -> String {
            self.said.push(prompt.to_owned());
            self.answers.pop_front().unwrap_or("").to_owned()
        }
    }

    struct FakeService {
        allowed: bool,
        ready_after: u32,
        asked: u32,
    }
    impl Service for FakeService {
        fn view(&mut self) -> Option<CallWifiView> {
            self.asked += 1;
            Some(CallWifiView {
                supported: true,
                phones: vec![CallWifiPhone {
                    device_id: "fold".into(),
                    name: "Fold5".into(),
                    allowed: self.allowed,
                    ready: self.allowed && self.asked > self.ready_after,
                    test: "not_run".into(),
                    ..Default::default()
                }],
                ..Default::default()
            })
        }
        fn allow(&mut self, id: &str) -> Result<(), String> {
            assert_eq!(id, "fold");
            self.allowed = true;
            Ok(())
        }
    }

    #[test]
    fn the_whole_walk_pairs_allows_starts_the_helper_and_waits_for_the_phone() {
        let mut phone = FakePhone::default();
        let mut io = Script {
            // continue, allow, pairing address (take the suggestion), code, press Enter after the switch
            answers: VecDeque::from(["y", "y", "", "123456", ""]),
            ..Default::default()
        };
        let mut svc = FakeService {
            allowed: false,
            ready_after: 3,
            asked: 0,
        };
        setup(&Opts::default(), &mut phone, &mut io, Some(&mut svc)).unwrap();
        assert!(svc.allowed && phone.paired && phone.helper);
        assert!(
            phone
                .log
                .iter()
                .any(|l| l == "pair 192.168.1.20:37123 123456")
        );
        let start = phone
            .log
            .iter()
            .find(|l| l.contains("app_process"))
            .unwrap();
        assert!(start.starts_with("-s 192.168.1.20:41234 shell "), "{start}");
        assert!(
            start.contains(
                "--connect 40123 --token t00112233445566778899aabbccddeeff --app-uid 10234"
            ),
            "{start}"
        );
        let text = io.said.join("\n");
        assert!(text.contains("switch is off"), "{text}");
        assert!(text.contains("reports its helper ready"), "{text}");
        assert!(text.contains("call-audio use"), "{text}");
    }

    #[test]
    fn it_stops_early_and_says_why() {
        // Declined at the first question: nothing at all is run.
        let mut phone = FakePhone::default();
        let mut io = Script {
            answers: VecDeque::from(["n"]),
            ..Default::default()
        };
        setup(&Opts::default(), &mut phone, &mut io, None).unwrap();
        assert!(phone.log.is_empty());
        // A release build is refused before anything starts.
        let mut phone = FakePhone {
            devices: vec!["R5CW1"],
            release_build: true,
            ..Default::default()
        };
        let mut io = Script::default();
        let e = setup(
            &Opts {
                yes: true,
                ..Default::default()
            },
            &mut phone,
            &mut io,
            None,
        )
        .unwrap_err();
        assert!(e.contains("release build"), "{e}");
        assert!(!phone.log.iter().any(|l| l.contains("app_process")));
        // A wrong pairing code never reaches adb.
        let mut phone = FakePhone::default();
        let mut io = Script {
            answers: VecDeque::from(["y", "", "12ab56"]),
            ..Default::default()
        };
        assert!(
            setup(&Opts::default(), &mut phone, &mut io, None)
                .unwrap_err()
                .contains("6 digits")
        );
        assert!(!phone.log.iter().any(|l| l.starts_with("pair")));
    }

    #[test]
    fn status_and_stop_use_the_connected_phone() {
        let mut phone = FakePhone {
            devices: vec!["R5CW1"],
            helper: true,
            ..Default::default()
        };
        let mut io = Script::default();
        setup(
            &Opts {
                status: true,
                ..Default::default()
            },
            &mut phone,
            &mut io,
            None,
        )
        .unwrap();
        assert!(io.said.iter().any(|l| l.contains("helper running")));
        setup(
            &Opts {
                stop: true,
                ..Default::default()
            },
            &mut phone,
            &mut io,
            None,
        )
        .unwrap();
        assert!(!phone.helper);
        assert!(phone.log.iter().any(|l| l.contains("callwifi.STOP")));
    }

    #[test]
    fn status_without_a_phone_does_not_start_the_pairing_walk() {
        let mut phone = FakePhone::default();
        let mut io = Script::default();
        let e = setup(
            &Opts {
                status: true,
                ..Default::default()
            },
            &mut phone,
            &mut io,
            None,
        )
        .unwrap_err();
        assert!(e.contains("not reachable over adb"), "{e}");
        assert!(io.said.is_empty(), "{:?}", io.said);
        assert!(
            !phone
                .log
                .iter()
                .any(|l| l.starts_with("pair") || l.starts_with("mdns"))
        );
    }
}
