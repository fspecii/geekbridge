// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-ctl call-audio`: the "Call audio on this computer" checklist as text, the same data
//! the Settings and Calls windows draw. Commands shown are for you to copy; nothing is run.
use bridge_core::call_audio::{Setup, StepState, TestPhase, Verdict};
use bridge_core::ipc::{self, Request, Response, Token};
use bridge_i18n::tr;
use std::fmt::Write as _;

fn mark(s: StepState) -> &'static str {
    match s {
        StepState::Ok => "[ok]     ",
        StepState::Todo => "[todo]   ",
        StepState::Waiting => "[waiting]",
        StepState::Unknown => "[unknown]",
    }
}

/// The checklist as lines: one per step, then the one-line fix and the command to copy.
pub fn describe(s: &Setup) -> String {
    let mut out = String::new();
    let headline = match s.verdict {
        Verdict::Ready => tr("callaudio.verdict.ready"),
        Verdict::Todo => tr("callaudio.verdict.todo"),
        Verdict::NoBluetooth => tr("callaudio.verdict.no_bluetooth"),
        Verdict::MacFallback => tr("callaudio.verdict.mac"),
        Verdict::Unsupported => tr("callaudio.verdict.unsupported"),
    };
    let _ = writeln!(out, "{}: {headline}", tr("callaudio.title"));
    if s.verdict == Verdict::MacFallback {
        let _ = writeln!(
            out,
            "{}\n  - {}\n  - {}",
            tr("callaudio.mac.body"),
            tr("callaudio.mac.option1"),
            tr("callaudio.mac.option2")
        );
    }
    for st in &s.steps {
        let _ = writeln!(out, "{} {}", mark(st.state), tr(st.id.key()));
        if let Some(f) = &st.fix
            && matches!(st.state, StepState::Todo | StepState::Unknown)
        {
            let _ = writeln!(out, "           {}", tr(f.kind.key()));
            if let Some(c) = &f.command {
                let _ = writeln!(out, "           $ {c}");
            }
        }
    }
    if s.platform == "windows" {
        let _ = writeln!(out, "{}", tr("callaudio.windows.guide"));
    }
    if s.verdict == Verdict::NoBluetooth {
        if s.platform == "windows" {
            let _ = writeln!(out, "{}", tr("callaudio.buy.windows"));
        } else {
            let _ = writeln!(out, "{}", tr("callaudio.buy"));
        }
        let _ = writeln!(out, "{}", bridge_calls::readiness::DOC);
    }
    let name = |list: &[bridge_core::call_audio::Device], id: &Option<String>| {
        list.iter()
            .find(|d| Some(&d.id) == id.as_ref())
            .map_or_else(
                || tr("callaudio.device.none").to_owned(),
                |d| d.label.clone(),
            )
    };
    if !s.outputs.is_empty() || !s.inputs.is_empty() {
        let _ = writeln!(
            out,
            "{}: {}",
            tr("callaudio.output"),
            name(&s.outputs, &s.chosen_output)
        );
        let _ = writeln!(
            out,
            "{}: {}",
            tr("callaudio.input"),
            name(&s.inputs, &s.chosen_input)
        );
    }
    if s.test.phase == TestPhase::Done {
        let _ = writeln!(
            out,
            "{}: {}",
            tr("callaudio.test.button"),
            if s.test.tone_played == Some(false) {
                tr("callaudio.test.tone_failed")
            } else if s.test.mic_opened == Some(false) {
                tr("callaudio.test.mic_failed")
            } else if s.test.heard() {
                tr("callaudio.test.heard")
            } else {
                tr("callaudio.test.silent")
            }
        );
    }
    out
}

fn ask(port: u16, token: &Token, r: &Request) -> Result<Setup, String> {
    match ipc::call(port, token, r).map_err(|e| e.to_string())? {
        Response::CallAudioSetup(s) => Ok(*s),
        Response::Error(e) => Err(e.message),
        _ => Err("unexpected reply from the service".into()),
    }
}

pub fn run(args: &[String], port: u16, token: &Token) -> Result<(), String> {
    let words: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    match words.as_slice() {
        [] => {
            print!("{}", describe(&ask(port, token, &Request::CallAudioSetup)?));
            Ok(())
        }
        ["test"] => {
            ask(port, token, &Request::CallAudioTest)?;
            eprintln!("{}", tr("callaudio.test.tone"));
            let mut last = None;
            for _ in 0..200 {
                std::thread::sleep(std::time::Duration::from_millis(150));
                let s = ask(port, token, &Request::CallAudioSetup)?;
                let done = s.test.phase == TestPhase::Done;
                last = Some(s);
                if done {
                    break;
                }
            }
            let s = last.ok_or("no answer")?;
            println!("peak level {} of 100", s.test.peak());
            print!("{}", describe(&s));
            Ok(())
        }
        ["use", rest @ ..] => {
            let (mut output, mut input) = (None, None);
            let mut it = rest.iter();
            while let Some(flag) = it.next() {
                let value = it.next().map(|v| (*v).to_owned());
                match *flag {
                    "--output" => output = value,
                    "--input" => input = value,
                    _ => return Err(format!("unknown option {flag}")),
                }
            }
            print!(
                "{}",
                describe(&ask(
                    port,
                    token,
                    &Request::CallAudioChoose { output, input }
                )?)
            );
            Ok(())
        }
        _ => Err(
            "usage: geekbridge-ctl call-audio [test | use [--output NAME] [--input NAME]]".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::call_audio::{LinuxFacts, build_linux};

    #[test]
    fn no_bluetooth_checklist_reads_plainly() {
        let s = build_linux(&LinuxFacts::default());
        let t = describe(&s);
        assert!(t.contains("No Bluetooth on this computer"), "{t}");
        assert!(t.contains("[todo]    Bluetooth adapter found"), "{t}");
        assert!(
            t.contains("no Bluetooth. Add a USB Bluetooth adapter"),
            "{t}"
        );
        assert!(t.contains("btusb"));
        assert!(t.contains("docs/call-audio-options.md"));
    }

    #[test]
    fn windows_checklist_speaks_windows_and_never_linux() {
        use bridge_core::call_audio::{PnpDevice, build_windows};
        let dev = |status: &str, name: &str, id: &str| PnpDevice {
            status: status.into(),
            name: name.into(),
            instance_id: id.into(),
        };
        let radio = dev(
            "OK",
            "Intel Wireless Bluetooth",
            "USB\\VID_8087&PID_0033\\5&1",
        );
        let node = |st: &str| {
            dev(
                st,
                "Pixel 8 Hands-Free AG",
                "BTHENUM\\{0000111F-0000-1000-8000-00805F9B34FB}_VID&0001004C_PID&0000\\7&2",
            )
        };
        let cases = [
            (
                build_windows(Some(&[]), &[]),
                "No Bluetooth on this computer",
            ),
            (
                build_windows(Some(std::slice::from_ref(&radio)), &[]),
                "Almost there",
            ),
            (
                build_windows(Some(&[radio.clone(), node("Unknown")]), &[]),
                "Almost there",
            ),
            (build_windows(Some(&[radio, node("OK")]), &[]), "Ready"),
            (build_windows(None, &[]), "Almost there"),
        ];
        for (setup, headline) in cases {
            let t = describe(&setup);
            assert!(t.contains(headline), "{headline}: {t}");
            assert!(t.contains("default communications device"), "{t}");
            assert!(t.contains("Settings"), "{t}");
            for linux in [
                "btusb",
                "pactl",
                "wpctl",
                "bluetoothctl",
                "systemctl",
                "PipeWire",
                "rfkill",
                "sudo",
            ] {
                assert!(!t.contains(linux), "{linux} in: {t}");
            }
        }
        let paired_off = build_windows(
            Some(&[dev("OK", "Intel Bluetooth", "USB\\VID_8087&PID_0033\\5&1")]),
            &[],
        );
        assert!(describe(&paired_off).contains("add your phone as a Bluetooth device"));
    }

    #[test]
    fn the_mac_checklist_says_answer_here_and_talk_on_the_phone() {
        let t = describe(&bridge_core::call_audio::mac_fallback());
        assert!(t.contains("Answer here and talk on the phone."), "{t}");
        for linux in ["btusb", "pactl", "bluetoothctl", "systemctl", "PipeWire"] {
            assert!(!t.contains(linux), "{linux} in: {t}");
        }
    }

    #[test]
    fn commands_are_shown_with_a_prompt_never_run() {
        let s = build_linux(&LinuxFacts {
            adapters: 1,
            powered: Some(false),
            rfkill: Some(Default::default()),
            ..LinuxFacts::default()
        });
        let t = describe(&s);
        assert!(t.contains("$ bluetoothctl power on"), "{t}");
    }
}
