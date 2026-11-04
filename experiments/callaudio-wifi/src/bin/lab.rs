// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-callaudio-lab`: run the EXPERIMENTAL call-audio path end to end in one process and print a JSON
//! report (latency, loss, codec cost, the dominant frequency that came out of each direction).
//!
//! ```text
//! geekbridge-callaudio-lab [--seconds S] [--latency-ms N] [--loss P] [--loss-penalty-ms N] [--seed N]
//!     [--down-tone HZ] [--up-tone HZ]
//!     [--helper HOST:PORT --source VOICE_DOWNLINK|VOICE_UPLINK|VOICE_CALL|REMOTE_SUBMIX|MIC|TONE --token T
//!      [--inject telephony_tx|voip_mix|none]]
//!     [--down-pw-target NODE] [--up-pw-target NODE] [--pw-runtime-dir DIR]
//! ```
//! `--helper` takes the far party's audio from the shell-uid helper (through `adb forward`) and sends the
//! computer's audio back to it for injection. `--down-pw-target`/`--up-pw-target` play the far party to a
//! PipeWire node and capture the computer's microphone from one; `--pw-runtime-dir` points at a private PipeWire
//! (the e2e script never uses the user's session). Without them everything stays in memory.
use std::time::Duration;

use bridge_callaudio::jitter::JitterConfig;
use bridge_callaudio::lab::{self, LabConfig, Sink, Source};
use bridge_callaudio::pcm::{PwEnv, PwSink, PwSource};
use bridge_callaudio::wire::Uplink;
use bridge_chaos::Faults;

fn usage() -> ! {
    eprintln!(
        "usage: geekbridge-callaudio-lab [--seconds S] [--latency-ms N] [--loss P] [--loss-penalty-ms N] [--seed N] \
         [--down-tone HZ] [--up-tone HZ] [--helper HOST:PORT --source NAME --token T [--inject ROUTE]] \
         [--down-pw-target NODE] [--up-pw-target NODE] [--pw-runtime-dir DIR] [--max-frames N]"
    );
    std::process::exit(2)
}

fn main() {
    let mut a = std::env::args().skip(1);
    let mut cfg = LabConfig::default();
    let (mut latency, mut loss, mut penalty) = (0u64, 0.0f64, 200u64);
    let (mut helper, mut source, mut token, mut inject) = (
        None,
        "VOICE_DOWNLINK".to_string(),
        String::new(),
        "none".to_string(),
    );
    let (mut down_target, mut up_target, mut runtime) =
        (None::<String>, None::<String>, None::<String>);
    let mut chaos = false;
    while let Some(k) = a.next() {
        let mut v = || a.next().unwrap_or_else(|| usage());
        match k.as_str() {
            "--seconds" => cfg.seconds = v().parse().unwrap_or_else(|_| usage()),
            "--latency-ms" => {
                latency = v().parse().unwrap_or_else(|_| usage());
                chaos = true;
            }
            "--loss" => {
                loss = v().parse().unwrap_or_else(|_| usage());
                chaos = true;
            }
            "--loss-penalty-ms" => penalty = v().parse().unwrap_or_else(|_| usage()),
            "--seed" => cfg.seed = v().parse().unwrap_or_else(|_| usage()),
            "--max-frames" => {
                cfg.jitter = JitterConfig {
                    max_frames: v().parse().unwrap_or_else(|_| usage()),
                    ..JitterConfig::default()
                }
            }
            "--down-tone" => {
                cfg.down_source = Source::Tone {
                    hz: v().parse().unwrap_or_else(|_| usage()),
                    amp: 0.3,
                }
            }
            "--up-tone" => {
                cfg.up_source = Source::Tone {
                    hz: v().parse().unwrap_or_else(|_| usage()),
                    amp: 0.3,
                }
            }
            "--helper" => helper = Some(v().parse().unwrap_or_else(|_| usage())),
            "--source" => source = v(),
            "--token" => token = v(),
            "--inject" => inject = v(),
            "--down-pw-target" => down_target = Some(v()),
            "--up-pw-target" => up_target = Some(v()),
            "--pw-runtime-dir" => runtime = Some(v()),
            _ => usage(),
        }
    }
    if chaos {
        cfg.faults = Some(Faults {
            latency: Duration::from_millis(latency),
            loss,
            loss_penalty: Duration::from_millis(penalty),
            ..Faults::default()
        });
    }
    let ok_name = |s: &str| {
        !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    if !ok_name(&source) || !(token.is_empty() || ok_name(&token)) {
        usage();
    }
    if let Some(addr) = helper {
        let cmd = format!(
            r#"{{"cmd":"start","source":"{source}","token":"{token}","inject":"{inject}"}}"#
        );
        cfg.down_source = Source::Helper(addr, cmd);
        cfg.up_sink = Sink::Helper;
        cfg.uplink = Uplink::from_name(&inject).unwrap_or_else(|| usage());
        if cfg.uplink == Uplink::None {
            // Still exercise the uplink path in the lab; the helper reports that it has nowhere to play it.
            cfg.uplink = Uplink::VoipMix;
        }
    }
    let env = PwEnv {
        runtime_dir: runtime,
        remote: None,
    };
    if let Some(t) = down_target {
        match PwSink::spawn(Some(&t), &env) {
            Ok(s) => cfg.down_sink = Sink::Blocking(Box::new(s)),
            Err(e) => {
                eprintln!("pw-cat playback: {e}");
                std::process::exit(1)
            }
        }
    }
    if let Some(t) = up_target {
        match PwSource::spawn(Some(&t), &env) {
            Ok(s) => cfg.up_source = Source::Blocking(Box::new(s)),
            Err(e) => {
                eprintln!("pw-cat record: {e}");
                std::process::exit(1)
            }
        }
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    match rt.block_on(lab::run(cfg)) {
        Ok(r) => println!("{}", serde_json::to_string_pretty(&r.json()).expect("json")),
        Err(e) => {
            eprintln!("lab failed: {e}");
            std::process::exit(1)
        }
    }
}
