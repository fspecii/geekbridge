//! Private in-memory audio server for the call-audio tests. It speaks only the `AudioServer`
//! trait, so no test here can touch a real sound server, move a stream or play a sound.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};

use bridge_calls::audio::model::{
    CardInfo, CardProfile, ModuleInfo, Props, ServerIdentity, ServerSnapshot, SinkInfo, SourceInfo,
};
use bridge_calls::audio::{AudioServer, ServerError};

pub const PHONE: &str = "AA:BB:CC:00:11:22";
pub const OTHER_PHONE: &str = "AA:BB:CC:99:99:99";
pub const CARD: &str = "bluez_card.AA_BB_CC_00_11_22";
pub const PHONE_SINK: &str = "bluez_output.AA_BB_CC_00_11_22.1";
pub const PHONE_SRC: &str = "bluez_input.AA_BB_CC_00_11_22.1";
pub const SPEAKER: &str = "alsa_output.pci-0000_00_1f.3.analog-stereo";
pub const MIC: &str = "alsa_input.pci-0000_00_1f.3.analog-stereo";
pub const USER_LOOPBACK_ARG: &str = "source=my_mic sink=my_speaker latency_msec=20";

pub fn props(pairs: &[(&str, &str)]) -> Props {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Snapshot,
    Load(String, String),
    Unload(u32),
    Reconnect,
}

#[derive(Clone, Copy, Debug)]
pub enum LoadFault {
    Reject,
    /// Timed out, and the module was *not* loaded.
    TimeoutNotLoaded,
    /// Timed out, but the server loaded it anyway (unknown outcome).
    TimeoutButLoaded,
}

#[derive(Clone, Copy, Debug)]
pub enum EchoBehavior {
    Immediately,
    AfterPolls(u32),
    Never,
    /// Nodes appear but belong to another module.
    ForeignOwner,
}

pub type FaultHook = Box<dyn FnMut(&str, &str) -> Option<LoadFault> + Send>;
pub type LoadedHook = Box<dyn FnMut(&mut ServerSnapshot, &str) + Send>;

pub struct State {
    pub snap: ServerSnapshot,
    pub next_index: u32,
    pub log: Vec<Call>,
    pub down: bool,
    pub reconnect_fails: bool,
    pub report_arguments: bool,
    pub echo: EchoBehavior,
    pub pending_echo: Option<(u32, String, String, u32)>,
    /// Decides a fault for `(module, argument)`.
    pub load_fault: Option<FaultHook>,
    pub fail_unload: BTreeSet<u32>,
    pub unload_connection_loss: bool,
    /// Runs after a module is loaded, with the server state (a peer changing mid-start).
    pub after_load: Option<LoadedHook>,
}

#[derive(Clone)]
pub struct Mock(pub Arc<Mutex<State>>);

impl Mock {
    pub fn new(snap: ServerSnapshot) -> Self {
        Self(Arc::new(Mutex::new(State {
            snap,
            next_index: 100,
            log: vec![],
            down: false,
            reconnect_fails: false,
            report_arguments: true,
            echo: EchoBehavior::Immediately,
            pending_echo: None,
            load_fault: None,
            fail_unload: BTreeSet::new(),
            unload_connection_loss: false,
            after_load: None,
        })))
    }
    pub fn st(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }
    pub fn log(&self) -> Vec<Call> {
        self.st().log.clone()
    }
    pub fn clear_log(&self) {
        self.st().log.clear();
    }
    pub fn loads(&self) -> Vec<(String, String)> {
        self.log()
            .into_iter()
            .filter_map(|c| {
                if let Call::Load(m, a) = c {
                    Some((m, a))
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn unloads(&self) -> Vec<u32> {
        self.log()
            .into_iter()
            .filter_map(|c| {
                if let Call::Unload(i) = c {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    }
    /// Any call that is not a read: the manager must make none of these before `start`.
    pub fn mutations(&self) -> usize {
        self.log()
            .iter()
            .filter(|c| matches!(c, Call::Load(..) | Call::Unload(_)))
            .count()
    }
    pub fn module_args(&self, name: &str) -> Vec<String> {
        self.st()
            .snap
            .modules
            .iter()
            .filter(|m| m.name == name)
            .map(|m| m.argument.clone().unwrap_or_default())
            .collect()
    }
    pub fn module_names(&self) -> Vec<(u32, String)> {
        self.st()
            .snap
            .modules
            .iter()
            .map(|m| (m.index, m.name.clone()))
            .collect()
    }
    pub fn has_module(&self, index: u32) -> bool {
        self.st().snap.modules.iter().any(|m| m.index == index)
    }
    /// A module disappears without an unload call (a loopback unloading itself).
    pub fn vanish_module(&self, index: u32) {
        self.st().snap.modules.retain(|m| m.index != index);
    }
    pub fn vanish_phone_nodes(&self) {
        let mut s = self.st();
        s.snap.sinks.retain(|x| x.name != PHONE_SINK);
        s.snap.sources.retain(|x| x.name != PHONE_SRC);
    }
    /// Server restarted: all modules gone, counters reset, new identity.
    pub fn restart(&self, cookie: u32) {
        let mut s = self.st();
        s.snap.modules.clear();
        s.snap.identity.cookie = cookie;
        s.next_index = 1;
        s.down = false;
    }
}

impl AudioServer for Mock {
    fn snapshot(&mut self) -> Result<ServerSnapshot, ServerError> {
        let mut s = self.st();
        s.log.push(Call::Snapshot);
        if s.down {
            return Err(ServerError::ConnectionLost("mock server down".into()));
        }
        if let Some((polls, src, snk, owner)) = s.pending_echo.take() {
            if polls == 0 {
                add_echo_nodes(&mut s.snap, &src, &snk, owner);
            } else {
                s.pending_echo = Some((polls - 1, src, snk, owner));
            }
        }
        let mut snap = s.snap.clone();
        if !s.report_arguments {
            snap.modules.iter_mut().for_each(|m| m.argument = None);
        }
        Ok(snap)
    }

    fn load_module(&mut self, name: &str, argument: &str) -> Result<u32, ServerError> {
        let mut s = self.st();
        s.log.push(Call::Load(name.into(), argument.into()));
        if s.down {
            return Err(ServerError::ConnectionLost("mock server down".into()));
        }
        let fault = s.load_fault.as_mut().and_then(|f| f(name, argument));
        if let Some(LoadFault::Reject) = fault {
            return Err(ServerError::Rejected {
                what: "load module",
                detail: "mock refusal".into(),
            });
        }
        if let Some(LoadFault::TimeoutNotLoaded) = fault {
            return Err(ServerError::Timeout {
                what: "load module",
            });
        }
        let arg_of = |key: &str| {
            argument
                .split_whitespace()
                .find_map(|t| t.strip_prefix(&format!("{key}=")).map(str::to_string))
        };
        if name == "module-loopback" {
            let (src, snk) = (
                arg_of("source").unwrap_or_default(),
                arg_of("sink").unwrap_or_default(),
            );
            let pending = s.pending_echo.as_ref().map(|p| (p.1.clone(), p.2.clone()));
            let known_src = s.snap.sources.iter().any(|x| x.name == src)
                || pending.as_ref().is_some_and(|p| p.0 == src);
            let known_snk = s.snap.sinks.iter().any(|x| x.name == snk)
                || pending.as_ref().is_some_and(|p| p.1 == snk);
            if !known_src || !known_snk {
                return Err(ServerError::Rejected {
                    what: "load module",
                    detail: "no such source or sink".into(),
                });
            }
        }
        let index = s.next_index;
        s.next_index += 1;
        s.snap.modules.push(ModuleInfo {
            index,
            name: name.into(),
            argument: Some(argument.into()),
        });
        if name == "module-echo-cancel" {
            let (src, snk) = (arg_of("source_name").unwrap(), arg_of("sink_name").unwrap());
            match s.echo {
                EchoBehavior::Immediately => add_echo_nodes(&mut s.snap, &src, &snk, index),
                EchoBehavior::AfterPolls(n) => s.pending_echo = Some((n, src, snk, index)),
                EchoBehavior::ForeignOwner => add_echo_nodes(&mut s.snap, &src, &snk, 1),
                EchoBehavior::Never => {}
            }
        }
        if let Some(mut hook) = s.after_load.take() {
            hook(&mut s.snap, argument);
            s.after_load = Some(hook);
        }
        if let Some(LoadFault::TimeoutButLoaded) = fault {
            return Err(ServerError::Timeout {
                what: "load module",
            });
        }
        Ok(index)
    }

    fn unload_module(&mut self, index: u32) -> Result<(), ServerError> {
        let mut s = self.st();
        s.log.push(Call::Unload(index));
        if s.down || s.unload_connection_loss {
            return Err(ServerError::ConnectionLost("mock server down".into()));
        }
        if s.fail_unload.contains(&index) {
            return Err(ServerError::Rejected {
                what: "unload module",
                detail: "mock refusal".into(),
            });
        }
        if !s.snap.modules.iter().any(|m| m.index == index) {
            return Err(ServerError::Rejected {
                what: "unload module",
                detail: "no such module".into(),
            });
        }
        s.snap.modules.retain(|m| m.index != index);
        s.snap.sinks.retain(|x| x.owner_module != Some(index));
        s.snap.sources.retain(|x| x.owner_module != Some(index));
        Ok(())
    }

    fn reconnect(&mut self) -> Result<(), ServerError> {
        let mut s = self.st();
        s.log.push(Call::Reconnect);
        if s.reconnect_fails {
            return Err(ServerError::Unavailable("mock: still down".into()));
        }
        s.down = false;
        Ok(())
    }
}

fn add_echo_nodes(snap: &mut ServerSnapshot, src: &str, snk: &str, owner: u32) {
    snap.sinks.push(SinkInfo {
        index: 500 + owner,
        name: snk.into(),
        description: "echo sink".into(),
        card: None,
        owner_module: Some(owner),
        props: Props::new(),
    });
    snap.sources.push(SourceInfo {
        index: 600 + owner,
        name: src.into(),
        description: "echo source".into(),
        card: None,
        owner_module: Some(owner),
        monitor_of_sink: None,
        props: Props::new(),
    });
}

/// A server with a phone in the given profile, a built-in speaker/mic, and two user modules.
pub fn server(active_profile: &str) -> ServerSnapshot {
    let voice = active_profile.starts_with("headset");
    let mut sinks = vec![SinkInfo {
        index: 1,
        name: SPEAKER.into(),
        description: "Built-in Audio Analog Stereo".into(),
        card: Some(1),
        owner_module: None,
        props: props(&[("device.api", "alsa")]),
    }];
    let mut sources = vec![
        SourceInfo {
            index: 2,
            name: MIC.into(),
            description: "Built-in Audio Mic".into(),
            card: Some(1),
            owner_module: None,
            monitor_of_sink: None,
            props: props(&[("device.api", "alsa")]),
        },
        SourceInfo {
            index: 3,
            name: format!("{SPEAKER}.monitor"),
            description: "Monitor of speaker".into(),
            card: Some(1),
            owner_module: None,
            monitor_of_sink: Some(1),
            props: Props::new(),
        },
    ];
    if voice {
        sinks.push(SinkInfo {
            index: 21,
            name: PHONE_SINK.into(),
            description: "Phone".into(),
            card: Some(7),
            owner_module: None,
            props: props(&[
                ("api.bluez5.address", PHONE),
                ("api.bluez5.profile", "headset-head-unit"),
            ]),
        });
        sources.push(SourceInfo {
            index: 22,
            name: PHONE_SRC.into(),
            description: "Phone".into(),
            card: Some(7),
            owner_module: None,
            monitor_of_sink: None,
            props: props(&[
                ("api.bluez5.address", PHONE),
                ("api.bluez5.profile", "headset-head-unit"),
            ]),
        });
    } else if active_profile == "a2dp-sink" {
        sinks.push(SinkInfo {
            index: 20,
            name: PHONE_SINK.into(),
            description: "Phone (media)".into(),
            card: Some(7),
            owner_module: None,
            props: props(&[
                ("api.bluez5.address", PHONE),
                ("api.bluez5.profile", "a2dp-sink"),
            ]),
        });
    }
    ServerSnapshot {
        identity: ServerIdentity {
            server_name: "PulseAudio (on PipeWire 1.6.9)".into(),
            server_version: "17.0.0".into(),
            host_name: "testhost".into(),
            cookie: 0xC0FFEE,
        },
        default_sink: Some(SPEAKER.into()),
        default_source: Some(MIC.into()),
        cards: vec![CardInfo {
            index: 7,
            name: CARD.into(),
            props: props(&[("api.bluez5.address", PHONE), ("device.api", "bluez5")]),
            profiles: [
                "a2dp-sink",
                "headset-head-unit-msbc",
                "headset-head-unit-cvsd",
                "off",
            ]
            .iter()
            .map(|n| CardProfile {
                name: (*n).into(),
                available: true,
            })
            .collect(),
            active_profile: Some(active_profile.into()),
        }],
        sinks,
        sources,
        modules: vec![
            ModuleInfo {
                index: 10,
                name: "module-loopback".into(),
                argument: Some(USER_LOOPBACK_ARG.into()),
            },
            ModuleInfo {
                index: 11,
                name: "module-null-sink".into(),
                argument: Some("sink_name=user_null".into()),
            },
        ],
    }
}
