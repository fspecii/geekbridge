// SPDX-License-Identifier: GPL-3.0-or-later
//! Host side of the "Call audio on this computer" setup: reads the machine into the checklist model
//! (`bridge_core::call_audio`), remembers which headphones and microphone were chosen, and runs the
//! local test (a short tone, then five seconds of microphone level).
//!
//! Everything here is read-only except the test, which plays a quiet tone and listens to the chosen
//! microphone without keeping any of it. No command is run through a shell, nothing uses `sudo`, and
//! the Bluetooth state is never changed: the window shows fixes to copy, the user runs them.
use bridge_core::call_audio::{
    self as model, Device, LinuxFacts, Rfkill, Selection, Setup, TestPhase, TestProgress,
};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// File name of the saved choice inside the state directory.
pub const SELECTION_FILE: &str = "call-audio.json";

/// Output of a command with a deadline; `None` on any failure.
pub fn run_text(program: &str, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    // Read on a helper thread so a command that never exits cannot hold the caller past `limit`.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.by_ref().take(1 << 20).read_to_end(&mut s);
        let _ = tx.send(s);
    });
    let got = rx.recv_timeout(limit).ok();
    if got.is_none() {
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    let bytes = got?;
    status
        .success()
        .then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads the machine. A trait so the service logic is testable without a Bluetooth stack.
pub trait Host: Send + Sync {
    fn collect(&self, selection: &Selection) -> Setup;
    /// Blocking: play the tone on `output` and meter `input`, reporting into `progress`.
    fn run_test(&self, output: Option<&str>, input: Option<&str>, progress: &Mutex<TestProgress>);
}

/// The real machine.
pub struct SystemHost;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Host for SystemHost {
    fn collect(&self, selection: &Selection) -> Setup {
        match std::env::consts::OS {
            "linux" => linux_setup(selection),
            "windows" => windows_setup(),
            "macos" => model::mac_fallback(),
            other => model::unsupported(other),
        }
    }

    fn run_test(&self, output: Option<&str>, input: Option<&str>, progress: &Mutex<TestProgress>) {
        run_local_test(&Tools::system(), output, input, progress);
    }
}

// ----------------------------------------------------------------------------------- Linux

const SHORT: Duration = Duration::from_secs(2);

fn rfkill_state(dir: &Path) -> Option<Rfkill> {
    let mut found: Option<Rfkill> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let read = |n: &str| std::fs::read_to_string(p.join(n)).unwrap_or_default();
        if let Some(r) = model::parse_rfkill(&read("type"), &read("soft"), &read("hard")) {
            let cur = found.get_or_insert_with(Rfkill::default);
            cur.soft |= r.soft;
            cur.hard |= r.hard;
        }
    }
    found
}

fn linux_setup(selection: &Selection) -> Setup {
    let adapters = bridge_calls::readiness::count_adapters(Path::new("/sys/class/bluetooth"));
    let bt = |args: &[&str]| run_text("bluetoothctl", args, SHORT);
    let mut f = LinuxFacts {
        adapters,
        selection: selection.clone(),
        ..LinuxFacts::default()
    };
    if adapters > 0 {
        f.rfkill = rfkill_state(Path::new("/sys/class/rfkill"));
        f.powered = bt(&["show"]).and_then(|s| model::parse_powered(&s));
        f.paired_phones = bt(&["devices", "Paired"]).map(|list| {
            bridge_calls::readiness::parse_devices(&list)
                .into_iter()
                .filter(|a| {
                    bt(&["info", a])
                        .is_some_and(|i| bridge_calls::readiness::info_is_hfp_gateway(&i))
                })
                .collect()
        });
        if let Some(phones) = &f.paired_phones {
            f.connected_phones = phones
                .iter()
                .filter(|a| bt(&["info", a]).is_some_and(|i| model::parse_connected(&i)))
                .count();
        }
        let probe = bridge_calls::probe::run(&bridge_calls::probe::ProbeOptions::default());
        f.registered_phones = probe.summary.phones;
    }
    let default_sink = run_text("pactl", &["get-default-sink"], SHORT).map(|s| s.trim().to_owned());
    let default_src =
        run_text("pactl", &["get-default-source"], SHORT).map(|s| s.trim().to_owned());
    let sinks = run_text("pactl", &["--format=json", "list", "sinks"], SHORT);
    let sources = run_text("pactl", &["--format=json", "list", "sources"], SHORT);
    f.audio_server_ok = sinks.is_some() && sources.is_some();
    f.outputs = sinks
        .map(|j| model::parse_pactl_devices(&j, default_sink.as_deref(), false))
        .unwrap_or_default();
    f.inputs = sources
        .map(|j| model::parse_pactl_devices(&j, default_src.as_deref(), true))
        .unwrap_or_default();
    model::build_linux(&f)
}

// --------------------------------------------------------------------------------- Windows

fn pnp(class: &str) -> Option<Vec<model::PnpDevice>> {
    let script = format!(
        "Get-PnpDevice -Class {class} | Select-Object Status,FriendlyName,InstanceId | ConvertTo-Json -Compress"
    );
    run_text(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
        Duration::from_secs(8),
    )
    .map(|t| model::parse_pnp_json(&t))
}

fn windows_setup() -> Setup {
    let bt = pnp("Bluetooth");
    let ep = pnp("AudioEndpoint").unwrap_or_default();
    model::build_windows(bt.as_deref(), &ep)
}

// ------------------------------------------------------------------------- the local test

/// The programs the test uses; replaceable so the logic is tested with stand-ins.
pub struct Tools {
    pub players: Vec<(String, Vec<String>)>,
    pub recorder: (String, Vec<String>),
}

impl Tools {
    pub fn system() -> Self {
        Self {
            // `{dev}` is replaced by the output name. PulseAudio's player first, PipeWire's second.
            players: vec![
                (
                    "paplay".into(),
                    vec!["--file-format=wav".into(), "--device={dev}".into()],
                ),
                ("pw-play".into(), vec!["--target={dev}".into(), "-".into()]),
            ],
            recorder: (
                "parec".into(),
                vec![
                    "--device={dev}".into(),
                    "--format=s16le".into(),
                    "--rate=16000".into(),
                    "--channels=1".into(),
                    "--raw".into(),
                    "--latency-msec=50".into(),
                ],
            ),
        }
    }
}

fn args_for(args: &[String], dev: Option<&str>) -> Vec<String> {
    args.iter()
        .filter_map(|a| match (a.contains("{dev}"), dev) {
            (true, Some(d)) => Some(a.replace("{dev}", d)),
            (true, None) => None,
            _ => Some(a.clone()),
        })
        .collect()
}

fn reap(mut c: Child) {
    let _ = c.kill();
    let _ = c.wait();
}

/// Play the tone with the first player that starts and finishes. `true` if one did.
fn play_tone(tools: &Tools, output: Option<&str>) -> bool {
    let wav = model::tone_wav();
    for (prog, args) in &tools.players {
        let Ok(mut child) = Command::new(prog)
            .args(args_for(args, output))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&wav); // closing it ends the stream
        }
        let end = Instant::now() + Duration::from_secs(4);
        loop {
            match child.try_wait() {
                Ok(Some(s)) => {
                    if s.success() {
                        return true;
                    }
                    break;
                }
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(30)),
                _ => {
                    reap(child);
                    break;
                }
            }
        }
    }
    false
}

/// Listen for [`model::TEST_SECONDS`], pushing one level per slice. `false` if the recorder would not start.
fn listen(tools: &Tools, input: Option<&str>, progress: &Mutex<TestProgress>) -> bool {
    let (prog, args) = &tools.recorder;
    let Ok(mut child) = Command::new(prog)
        .args(args_for(args, input))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let Some(mut out) = child.stdout.take() else {
        reap(child);
        return false;
    };
    let total = (model::TEST_SECONDS * model::METER_HZ) as usize;
    let end = Instant::now() + Duration::from_secs(u64::from(model::TEST_SECONDS) + 2);
    let mut buf = vec![0u8; model::slice_bytes()];
    let mut got_any = false;
    let mut levels = 0;
    // Blocking reads happen on a helper thread so a recorder that never produces data cannot hang the test.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let size = buf.len();
    std::thread::spawn(move || {
        loop {
            let mut b = vec![0u8; size];
            if out.read_exact(&mut b).is_err() || tx.send(b).is_err() {
                return;
            }
        }
    });
    while levels < total {
        let left = end.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.min(Duration::from_millis(500))) {
            Ok(b) => {
                buf.copy_from_slice(&b);
                got_any = true;
                levels += 1;
                lock(progress).levels.push(model::level_of(&buf));
            }
            Err(_) if Instant::now() >= end => break,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(_) => {}
        }
    }
    reap(child);
    got_any
}

pub fn run_local_test(
    tools: &Tools,
    output: Option<&str>,
    input: Option<&str>,
    progress: &Mutex<TestProgress>,
) {
    {
        let mut p = lock(progress);
        *p = TestProgress {
            phase: TestPhase::Tone,
            ..TestProgress::default()
        };
    }
    let tone = output.is_some() && play_tone(tools, output);
    {
        let mut p = lock(progress);
        p.tone_played = Some(tone);
        p.phase = TestPhase::Listening;
    }
    let mic = input.is_some() && listen(tools, input, progress);
    let mut p = lock(progress);
    p.mic_opened = Some(mic);
    p.phase = TestPhase::Done;
}

// ------------------------------------------------------------------------------ the service

/// What the daemon owns: the saved choice, the host and the running test.
pub struct CallAudioService {
    host: Box<dyn Host>,
    file: Option<PathBuf>,
    selection: Mutex<Selection>,
    test: Arc<Mutex<TestProgress>>,
    gate: Mutex<()>,
}

impl CallAudioService {
    pub fn new(host: Box<dyn Host>, file: Option<PathBuf>) -> Arc<Self> {
        let selection = file
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Selection::parse(&t))
            .unwrap_or_default();
        Arc::new(Self {
            host,
            file,
            selection: Mutex::new(selection),
            test: Arc::new(Mutex::new(TestProgress::default())),
            gate: Mutex::new(()),
        })
    }

    pub fn selection(&self) -> Selection {
        lock(&self.selection).clone()
    }

    /// The checklist with the test's current progress. Blocking (reads the machine).
    pub fn setup(&self) -> Setup {
        let mut s = self.host.collect(&self.selection());
        s.test = lock(&self.test).clone();
        s
    }

    /// Save the choice. Names must be devices the host lists right now; anything else is refused.
    pub fn choose(&self, output: Option<String>, input: Option<String>) -> Result<Setup, String> {
        let cur = self.host.collect(&self.selection());
        let known = |list: &[Device], n: &Option<String>| {
            n.as_ref()
                .is_none_or(|n| model::device_name_ok(n) && list.iter().any(|d| &d.id == n))
        };
        if !known(&cur.outputs, &output) || !known(&cur.inputs, &input) {
            return Err("that device is not listed".into());
        }
        let new = Selection { output, input };
        if let Some(path) = &self.file {
            write_private(path, new.to_json().as_bytes())
                .map_err(|_| "could not save the choice")?;
        }
        *lock(&self.selection) = new;
        Ok(self.setup())
    }

    /// Start the test on the chosen devices unless one is already running.
    pub fn start_test(self: &Arc<Self>) -> Result<Setup, String> {
        let _g = lock(&self.gate);
        let setup = self.host.collect(&self.selection());
        if !setup.can_test {
            return Err("call audio cannot be tested on this computer".into());
        }
        if matches!(
            lock(&self.test).phase,
            TestPhase::Tone | TestPhase::Listening
        ) {
            return Ok(self.setup());
        }
        *lock(&self.test) = TestProgress {
            phase: TestPhase::Tone,
            ..TestProgress::default()
        };
        let me = self.clone();
        let (out, inp) = (setup.chosen_output.clone(), setup.chosen_input.clone());
        std::thread::Builder::new()
            .name("geekbridge-call-audio-test".into())
            .spawn(move || me.host.run_test(out.as_deref(), inp.as_deref(), &me.test))
            .map_err(|_| "could not start the test")?;
        let mut s = setup;
        s.test = lock(&self.test).clone();
        Ok(s)
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::call_audio::{StepState, Verdict};

    struct Fake {
        outputs: Vec<Device>,
        inputs: Vec<Device>,
    }

    fn dev(id: &str) -> Device {
        Device {
            id: id.into(),
            label: id.into(),
            bluetooth: false,
            default: false,
        }
    }

    impl Host for Fake {
        fn collect(&self, sel: &Selection) -> Setup {
            model::build_linux(&LinuxFacts {
                adapters: 0,
                outputs: self.outputs.clone(),
                inputs: self.inputs.clone(),
                selection: sel.clone(),
                audio_server_ok: true,
                ..LinuxFacts::default()
            })
        }
        fn run_test(&self, _: Option<&str>, _: Option<&str>, p: &Mutex<TestProgress>) {
            let mut g = lock(p);
            g.levels = vec![40];
            g.phase = TestPhase::Done;
        }
    }

    fn service(dir: &Path) -> Arc<CallAudioService> {
        CallAudioService::new(
            Box::new(Fake {
                outputs: vec![dev("o1"), dev("o2")],
                inputs: vec![dev("i1")],
            }),
            Some(dir.join(SELECTION_FILE)),
        )
    }

    #[test]
    fn choice_is_validated_saved_private_and_reloaded() {
        let d = tempfile::tempdir().unwrap();
        let s = service(d.path());
        assert!(s.choose(Some("nope".into()), None).is_err());
        assert!(s.choose(Some("-evil".into()), None).is_err());
        let after = s.choose(Some("o2".into()), Some("i1".into())).unwrap();
        assert_eq!(after.chosen_output.as_deref(), Some("o2"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = d.path().join(SELECTION_FILE);
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let again = service(d.path());
        assert_eq!(again.selection().output.as_deref(), Some("o2"));
        // Clearing returns to the system default.
        again.choose(None, None).unwrap();
        assert_eq!(service(d.path()).selection(), Selection::default());
    }

    #[test]
    fn the_checklist_reports_no_bluetooth_but_still_allows_the_headset_test() {
        let d = tempfile::tempdir().unwrap();
        let s = service(d.path()).setup();
        assert_eq!(s.verdict, Verdict::NoBluetooth);
        assert_eq!(s.steps[5].state, StepState::Ok);
        assert!(s.can_test);
    }

    #[test]
    fn start_test_runs_once_and_progress_shows_in_the_setup() {
        let d = tempfile::tempdir().unwrap();
        let s = service(d.path());
        let first = s.start_test().unwrap();
        assert_eq!(first.test.phase, TestPhase::Tone);
        for _ in 0..100 {
            if s.setup().test.phase == TestPhase::Done {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let t = s.setup().test;
        assert_eq!(t.phase, TestPhase::Done);
        assert_eq!(t.levels, [40]);
    }

    #[test]
    fn test_is_refused_when_nothing_can_be_tested() {
        let d = tempfile::tempdir().unwrap();
        let s = CallAudioService::new(
            Box::new(Fake {
                outputs: vec![],
                inputs: vec![],
            }),
            Some(d.path().join(SELECTION_FILE)),
        );
        assert!(s.start_test().is_err());
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn local_test_with_stand_in_programs_meters_and_reports() {
        let d = tempfile::tempdir().unwrap();
        // Player: swallow stdin and succeed, recording its arguments. Recorder: a loud burst.
        let log = d.path().join("args");
        let player = script(
            d.path(),
            "play",
            &format!("echo \"$@\" > {}; cat >/dev/null", log.display()),
        );
        let rec = script(
            d.path(),
            "rec",
            "head -c 32000 /dev/zero | tr '\\0' '\\100' ; sleep 5",
        );
        let tools = Tools {
            players: vec![(player, vec!["--device={dev}".into()])],
            recorder: (rec, vec![]),
        };
        let progress = Mutex::new(TestProgress::default());
        let started = Instant::now();
        run_local_test(&tools, Some("speaker"), Some("mic"), &progress);
        let p = lock(&progress).clone();
        assert_eq!(p.phase, TestPhase::Done);
        assert_eq!(p.tone_played, Some(true));
        assert_eq!(p.mic_opened, Some(true));
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            "--device=speaker"
        );
        assert_eq!(p.levels.len(), 10, "32000 bytes is ten 100 ms slices");
        assert!(p.heard(), "{:?}", p.levels);
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[cfg(unix)]
    #[test]
    fn local_test_survives_missing_and_failing_programs() {
        let d = tempfile::tempdir().unwrap();
        let bad = script(d.path(), "bad", "exit 1");
        let tools = Tools {
            players: vec![
                ("/nonexistent/player".into(), vec![]),
                (bad.clone(), vec![]),
            ],
            recorder: ("/nonexistent/recorder".into(), vec![]),
        };
        let progress = Mutex::new(TestProgress::default());
        run_local_test(&tools, Some("a"), Some("b"), &progress);
        let p = lock(&progress).clone();
        assert_eq!((p.tone_played, p.mic_opened), (Some(false), Some(false)));
        assert_eq!(p.phase, TestPhase::Done);
        // A recorder that never sends anything ends by the deadline, not never.
        let silent = script(d.path(), "silent", "sleep 30");
        let tools = Tools {
            players: vec![],
            recorder: (silent, vec![]),
        };
        let started = Instant::now();
        run_local_test(
            &tools,
            None,
            Some("b"),
            &Mutex::new(TestProgress::default()),
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn device_placeholder_is_dropped_when_there_is_no_name() {
        let a = vec!["--device={dev}".to_owned(), "--raw".to_owned()];
        assert_eq!(args_for(&a, Some("x")), ["--device=x", "--raw"]);
        assert_eq!(args_for(&a, None), ["--raw"]);
    }

    #[test]
    fn rfkill_tree_is_combined_and_ignores_other_radios() {
        let d = tempfile::tempdir().unwrap();
        for (n, ty, soft, hard) in [
            ("rfkill0", "wlan", "1", "1"),
            ("rfkill1", "bluetooth", "1", "0"),
        ] {
            let p = d.path().join(n);
            std::fs::create_dir(&p).unwrap();
            std::fs::write(p.join("type"), format!("{ty}\n")).unwrap();
            std::fs::write(p.join("soft"), format!("{soft}\n")).unwrap();
            std::fs::write(p.join("hard"), format!("{hard}\n")).unwrap();
        }
        assert_eq!(
            rfkill_state(d.path()),
            Some(Rfkill {
                soft: true,
                hard: false
            })
        );
        assert_eq!(rfkill_state(Path::new("/nonexistent")), None);
    }
}
