// SPDX-License-Identifier: GPL-3.0-or-later
//! The loopback lab (EXPERIMENTAL, test and evidence tool): a "phone" and a "computer" in one process, joined by
//! the real GeekBridge link code (`bridge-kde` endpoints, TLS with pinned certificates generated for the run),
//! optionally through the `bridge-chaos` fault proxy, carrying `geekbridge.callaudio.*` packets through the
//! consent gate, the Opus codec and the jitter buffer in both directions.
//!
//! Sources and sinks are pluggable: a tone, the shell-uid helper on an emulator or phone (through
//! `adb forward`), PipeWire nodes, or meters that keep only numbers. End-to-end latency is measured from the
//! frame's capture stamp to the moment its slot leaves the jitter buffer for the sink (the sink's own output
//! buffer, and on a phone the AudioRecord buffer before the stamp, are not included; both are reported apart).
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bridge_chaos::{FaultProxy, Faults};
use bridge_kde::{
    DeviceCredentials, DeviceType, Endpoint, Handshake, LinkEvent, MemoryTrustStore, Packet,
    TransportConfig, TrustStore, TrustedLink, TrustedPeer, cert::unix_now,
};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::codec::{Decoder, Encoder};
use crate::gate::{Action, Gate, Side};
use crate::helper::{self, HelperFrame};
use crate::jitter::{JitterBuffer, JitterConfig, Out};
use crate::pcm::{MeterSink, PcmSink, PcmSource};
use crate::tone::{self, Tone};
use crate::wire::{self, Dir, FRAME_MS, Frame, Msg, RATE, Uplink};

/// Where a direction's audio comes from.
pub enum Source {
    Tone {
        hz: f64,
        amp: f64,
    },
    /// The shell-uid helper's PCM stream (already started by the caller), e.g. `adb forward tcp:N localabstract:…`.
    Helper(SocketAddr, String),
    /// A blocking PCM source (PipeWire) on its own thread.
    Blocking(Box<dyn PcmSource>),
    Silence,
}

/// Where a direction's decoded audio goes.
pub enum Sink {
    Meter,
    Blocking(Box<dyn PcmSink>),
    /// Back to the helper as INJECT frames (the phone-side uplink route under test).
    Helper,
}

pub struct LabConfig {
    pub seconds: f64,
    pub faults: Option<Faults>,
    pub seed: u64,
    pub down_source: Source,
    pub up_source: Source,
    pub down_sink: Sink,
    pub up_sink: Sink,
    pub jitter: JitterConfig,
    pub uplink: Uplink,
}

impl Default for LabConfig {
    fn default() -> Self {
        Self {
            seconds: 3.0,
            faults: None,
            seed: 1,
            down_source: Source::Tone {
                hz: 440.0,
                amp: 0.3,
            },
            up_source: Source::Tone {
                hz: 1000.0,
                amp: 0.3,
            },
            down_sink: Sink::Meter,
            up_sink: Sink::Meter,
            jitter: JitterConfig::default(),
            uplink: Uplink::TelephonyTx,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DirReport {
    pub sent: u64,
    pub bytes: u64,
    pub encode_us: u64,
    pub decode_us: u64,
    pub played: u64,
    pub concealed: u64,
    pub idle: u64,
    pub latency_ms: Vec<u32>,
    pub jitter: crate::jitter::JitterStats,
    pub jitter_est_ms: f64,
    pub dominant_hz: f64,
    pub rms: f64,
    pub gate_dropped: u64,
}

fn pct(v: &[u32], p: f64) -> u32 {
    if v.is_empty() {
        return 0;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    s[((s.len() - 1) as f64 * p).round() as usize]
}

impl DirReport {
    pub fn json(&self) -> Value {
        let per = |x: u64, n: u64| x.checked_div(n).unwrap_or(0);
        json!({
            "sent": self.sent,
            "avgBytes": per(self.bytes, self.sent),
            "kbps": if self.sent == 0 { 0.0 } else { self.bytes as f64 * 8.0 / (self.sent as f64 * f64::from(FRAME_MS)) },
            "encodeUsPerFrame": per(self.encode_us, self.sent),
            "decodeUsPerFrame": per(self.decode_us, self.played + self.concealed),
            "played": self.played,
            "concealed": self.concealed,
            "idleSlots": self.idle,
            "latencyMs": {"p50": pct(&self.latency_ms, 0.5), "p95": pct(&self.latency_ms, 0.95), "max": pct(&self.latency_ms, 1.0), "n": self.latency_ms.len()},
            "jitterBuffer": {"got": self.jitter.got, "lost": self.jitter.lost, "late": self.jitter.late, "shed": self.jitter.shed, "underruns": self.jitter.underruns, "rebuffers": self.jitter.rebuffers, "estJitterMs": (self.jitter_est_ms * 10.0).round() / 10.0},
            "dominantHz": self.dominant_hz.round(),
            "rms": self.rms.round(),
            "gateDropped": self.gate_dropped,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct LabReport {
    pub down: DirReport,
    pub up: DirReport,
    pub cpu_ms: u64,
    pub wall_ms: u64,
    pub tls: String,
    pub helper_hello: Option<String>,
    pub helper_status: Vec<String>,
}

impl LabReport {
    pub fn json(&self) -> Value {
        json!({
            "down": self.down.json(),
            "up": self.up.json(),
            "cpuMs": self.cpu_ms,
            "wallMs": self.wall_ms,
            "cpuPercentOfOneCore": if self.wall_ms == 0 { 0.0 } else { (self.cpu_ms as f64 * 1000.0 / self.wall_ms as f64).round() / 10.0 },
            "tls": self.tls,
            "helperHello": self.helper_hello,
            "helperStatus": self.helper_status,
        })
    }
}

/// Process CPU time (user + system) in ms; Linux only, 0 elsewhere.
fn cpu_ms() -> u64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/stat") else {
        return 0;
    };
    let Some(rest) = s.rsplit(')').next() else {
        return 0;
    };
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tick = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    // fields after ")": state=0 … utime=11 stime=12; clock ticks are 100 Hz on Linux.
    (tick(11) + tick(12)) * 10
}

fn trusted(of: &DeviceCredentials, name: &str, kind: &str) -> TrustedPeer {
    TrustedPeer {
        device_id: of.device_id().clone(),
        name: name.into(),
        device_type: kind.into(),
        protocol_version: bridge_kde::PROTOCOL_VERSION,
        cert: of.cert().clone(),
        paired_at_unix: unix_now(),
    }
}

/// Two endpoints that already trust each other, connected (through the fault proxy when given).
pub async fn linked_pair(
    faults: Option<Faults>,
    seed: u64,
) -> Result<(TrustedLink, TrustedLink, Option<FaultProxy>), String> {
    let e = |x: bridge_kde::KdeError| x.to_string();
    let phone_creds = Arc::new(DeviceCredentials::generate().map_err(e)?);
    let pc_creds = Arc::new(DeviceCredentials::generate().map_err(e)?);
    let phone_store = Arc::new(MemoryTrustStore::new());
    let pc_store = Arc::new(MemoryTrustStore::new());
    phone_store
        .insert(trusted(&pc_creds, "Lab computer", "desktop"))
        .map_err(e)?;
    pc_store
        .insert(trusted(&phone_creds, "Lab phone", "phone"))
        .map_err(e)?;
    // Loopback only: the lab never listens beyond 127.0.0.1.
    let config = || {
        let mut c = TransportConfig::default();
        c.address_policy.loopback = true;
        c.address_policy.local_networks = false;
        c
    };
    let caps = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let phone = Endpoint::new(
        phone_creds,
        "Lab phone",
        DeviceType::Phone,
        caps(&wire::PHONE_INCOMING),
        caps(&wire::COMPUTER_INCOMING),
        phone_store,
        config(),
    )
    .map_err(e)?;
    let pc = Endpoint::new(
        pc_creds,
        "Lab computer",
        DeviceType::Desktop,
        caps(&wire::COMPUTER_INCOMING),
        caps(&wire::PHONE_INCOMING),
        pc_store,
        config(),
    )
    .map_err(e)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|x| x.to_string())?;
    let phone_addr = listener.local_addr().map_err(|x| x.to_string())?;
    let proxy = match faults {
        Some(f) => Some(FaultProxy::start_with(phone_addr, f, seed).await),
        None => None,
    };
    let dial = proxy.as_ref().map_or(phone_addr, FaultProxy::addr);
    let phone_id = phone.local_identity().device_id.clone();
    let accept = tokio::spawn(async move {
        let (s, _) = listener.accept().await.map_err(|x| x.to_string())?;
        // Low latency matters more than throughput for 20 ms frames.
        let _ = s.set_nodelay(true);
        phone.accept(s).await.map_err(|x| x.to_string())
    });
    let pc_hs = pc.connect(dial, &phone_id).await.map_err(e)?;
    let phone_hs = accept.await.map_err(|x| x.to_string())??;
    match (phone_hs, pc_hs) {
        (Handshake::Trusted(p), Handshake::Trusted(c)) => Ok((p, c, proxy)),
        _ => Err("the pre-trusted endpoints did not come up trusted".into()),
    }
}

/// Pump a link: packets out from `out`, `geekbridge.callaudio.*` packets in to `inbound`.
fn pump(
    mut link: TrustedLink,
    mut out: mpsc::Receiver<Packet>,
    inbound: mpsc::Sender<Msg>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                ev = link.recv() => match ev {
                    Ok(LinkEvent::Packet(p)) => {
                        if let Ok(m) = wire::parse(&p.kind, &p.body) && inbound.send(m).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                p = out.recv() => match p {
                    Some(p) => if link.send(&p).await.is_err() { break },
                    None => break,
                },
            }
        }
    })
}

fn packet(m: &Msg) -> Packet {
    Packet::new(m.kind(), wire::body(m))
}

/// Feed `tx` with 20 ms frames from `src`, stamping each with the lab clock.
fn spawn_source(
    src: Source,
    epoch: Instant,
    tx: mpsc::Sender<(u32, Vec<i16>)>,
    helper_up: Option<mpsc::Receiver<HelperFrame>>,
    info: mpsc::Sender<(bool, String)>,
) {
    let stamp = move || epoch.elapsed().as_millis() as u32;
    match src {
        Source::Silence => {}
        Source::Tone { hz, amp } => {
            tokio::spawn(async move {
                let mut t = Tone::new(hz, amp);
                let mut iv = tokio::time::interval(Duration::from_millis(u64::from(FRAME_MS)));
                loop {
                    iv.tick().await;
                    if tx.send((stamp(), t.frame())).await.is_err() {
                        break;
                    }
                }
            });
        }
        Source::Blocking(mut s) => {
            std::thread::spawn(move || {
                let mut buf = vec![0i16; wire::FRAME_SAMPLES];
                while s.read(&mut buf).is_ok() {
                    if tx.blocking_send((stamp(), buf.clone())).is_err() {
                        break;
                    }
                }
            });
        }
        Source::Helper(addr, start_cmd) => {
            tokio::spawn(async move {
                let Ok(s) = TcpStream::connect(addr).await else {
                    let _ = info
                        .send((false, "{\"error\":\"helper connect failed\"}".into()))
                        .await;
                    return;
                };
                let _ = s.set_nodelay(true);
                let (mut r, mut w) = s.into_split();
                if helper::write_frame(&mut w, &HelperFrame::Cmd(start_cmd))
                    .await
                    .is_err()
                {
                    return;
                }
                if let Some(mut up) = helper_up {
                    tokio::spawn(async move {
                        while let Some(f) = up.recv().await {
                            if helper::write_frame(&mut w, &f).await.is_err() {
                                break;
                            }
                        }
                    });
                }
                while let Ok(f) = helper::read_frame(&mut r).await {
                    match f {
                        HelperFrame::Pcm { samples, .. } => {
                            if tx.send((stamp(), samples)).await.is_err() {
                                break;
                            }
                        }
                        HelperFrame::Hello(s) => {
                            let _ = info.send((true, s)).await;
                        }
                        HelperFrame::Status(s) => {
                            let _ = info.send((false, s)).await;
                        }
                        _ => {}
                    }
                }
            });
        }
    }
}

struct Receiver {
    jb: JitterBuffer<(u32, Vec<u8>)>,
    dec: Decoder,
    sink: Sink,
    meter: MeterSink,
    rep: DirReport,
}

impl Receiver {
    fn new(cfg: JitterConfig, sink: Sink) -> Self {
        Self {
            jb: JitterBuffer::new(cfg),
            dec: Decoder::new().expect("decoder"),
            sink,
            meter: MeterSink::new(RATE as usize * 2),
            rep: DirReport::default(),
        }
    }

    fn push(&mut self, f: Frame, epoch: Instant) {
        let now = epoch.elapsed().as_millis() as u64;
        self.jb.push(f.seq, (f.cap, f.opus), now);
    }

    /// One 20 ms slot. Returns decoded PCM for a helper sink.
    fn tick(&mut self, epoch: Instant) -> Option<Vec<i16>> {
        let slot = self.jb.pop();
        let now = epoch.elapsed().as_millis() as u32;
        let plain = match slot {
            Out::Idle => {
                self.rep.idle += 1;
                Out::Idle
            }
            Out::Packet((cap, p)) => {
                self.rep.played += 1;
                self.rep.latency_ms.push(now.wrapping_sub(cap));
                Out::Packet(p)
            }
            Out::Conceal { fec } => {
                self.rep.concealed += 1;
                Out::Conceal {
                    fec: fec.map(|(_, p)| p),
                }
            }
        };
        let t0 = Instant::now();
        let pcm = self.dec.render(&plain).to_vec();
        if !matches!(plain, Out::Idle) {
            self.rep.decode_us += t0.elapsed().as_micros() as u64;
        }
        let _ = self.meter.write(&pcm);
        match &mut self.sink {
            Sink::Meter => None,
            Sink::Blocking(s) => {
                let _ = s.write(&pcm);
                None
            }
            Sink::Helper => Some(pcm),
        }
    }

    fn finish(mut self) -> DirReport {
        self.rep.jitter = self.jb.stats();
        self.rep.jitter_est_ms = self.jb.jitter_ms();
        // Analyse the last second that was played, ignoring the start-up.
        let w = &self.meter.window;
        let tail = &w[w.len().saturating_sub(RATE as usize)..];
        self.rep.dominant_hz = tone::dominant_hz(tail, RATE);
        self.rep.rms = tone::rms(tail);
        self.rep
    }
}

/// Run the lab for `cfg.seconds`. Both gates start with every consent given and a call marked active, then the
/// phone presses start, as a user would; nothing flows before the computer's answer.
pub async fn run(cfg: LabConfig) -> Result<LabReport, String> {
    let epoch = Instant::now();
    let cpu0 = cpu_ms();
    let (phone_link, pc_link, _proxy) = linked_pair(cfg.faults.clone(), cfg.seed).await?;
    let tls = format!(
        "{} {}",
        pc_link.info().tls_version,
        pc_link.info().cipher_suite
    );
    let (p_out_tx, p_out_rx) = mpsc::channel::<Packet>(512);
    let (c_out_tx, c_out_rx) = mpsc::channel::<Packet>(512);
    let (p_in_tx, mut p_in) = mpsc::channel::<Msg>(512);
    let (c_in_tx, mut c_in) = mpsc::channel::<Msg>(512);
    let pump_p = pump(phone_link, p_out_rx, p_in_tx);
    let pump_c = pump(pc_link, c_out_rx, c_in_tx);

    let (down_tx, mut down_rx) = mpsc::channel::<(u32, Vec<i16>)>(64);
    let (up_tx, mut up_rx) = mpsc::channel::<(u32, Vec<i16>)>(64);
    let (info_tx, mut info_rx) = mpsc::channel::<(bool, String)>(64);
    let helper_inject = matches!(cfg.up_sink, Sink::Helper);
    let (inj_tx, inj_rx) = mpsc::channel::<HelperFrame>(64);
    spawn_source(
        cfg.down_source,
        epoch,
        down_tx,
        helper_inject.then_some(inj_rx),
        info_tx.clone(),
    );
    spawn_source(cfg.up_source, epoch, up_tx, None, info_tx);

    let mut pg = Gate::new(Side::Phone);
    let mut cg = Gate::new(Side::Computer);
    pg.set_consent(false, true);
    cg.set_consent(true, false);
    pg.set_call_active(true);
    // The lab exercises both directions: the phone allows speaking and the computer asks once the session is live.
    pg.uplink_route = cfg.uplink;
    pg.set_speak_allowed(cfg.uplink != Uplink::None);

    let mut down_enc = Encoder::new().map_err(|e| e.to_string())?;
    let mut up_enc = Encoder::new().map_err(|e| e.to_string())?;
    let mut down_rx_side = Receiver::new(cfg.jitter, cfg.down_sink);
    let mut up_rx_side = Receiver::new(cfg.jitter, cfg.up_sink);
    let (mut dseq, mut useq, mut inj_seq) = (0u32, 0u32, 0u32);
    let session = 1;

    let do_actions = |acts: Vec<Action>, out: &mpsc::Sender<Packet>, rx: &mut Receiver| {
        for a in acts {
            match a {
                Action::Send(m) => {
                    let _ = out.try_send(packet(&m));
                }
                Action::Accept(f) => rx.push(f, epoch),
                Action::Drop(_) => rx.rep.gate_dropped += 1,
                Action::IndicatorOn
                | Action::IndicatorOff
                | Action::PeerReady(_)
                | Action::SpeakState(_) => {}
            }
        }
    };
    do_actions(pg.start(session, cfg.uplink), &p_out_tx, &mut up_rx_side);
    let mut asked_speak = false;

    let mut report = LabReport {
        tls,
        ..Default::default()
    };
    let mut tick = tokio::time::interval(Duration::from_millis(u64::from(FRAME_MS)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let end = tokio::time::Instant::now() + Duration::from_secs_f64(cfg.seconds);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(end) => break,
            Some(m) = p_in.recv() => {
                let a = pg.on_msg(m, epoch.elapsed().as_millis() as u64);
                do_actions(a, &p_out_tx, &mut up_rx_side);
            }
            Some(m) = c_in.recv() => {
                let a = cg.on_msg(m, epoch.elapsed().as_millis() as u64);
                let live = a.contains(&Action::IndicatorOn);
                do_actions(a, &c_out_tx, &mut down_rx_side);
                if live && !asked_speak && cfg.uplink != Uplink::None {
                    asked_speak = true;
                    do_actions(cg.ask_speak(true), &c_out_tx, &mut down_rx_side);
                }
            }
            Some((cap, pcm)) = down_rx.recv() => {
                if pg.may_send(Dir::Down) {
                    let t0 = Instant::now();
                    if let Ok(opus) = down_enc.encode(&pcm) {
                        down_rx_side.rep.encode_us += t0.elapsed().as_micros() as u64;
                        down_rx_side.rep.bytes += opus.len() as u64;
                        down_rx_side.rep.sent += 1;
                        let f = Msg::Frame(Frame { session, dir: Dir::Down, seq: dseq, ts: dseq.wrapping_mul(wire::FRAME_SAMPLES as u32), cap, opus });
                        dseq = dseq.wrapping_add(1);
                        let _ = p_out_tx.try_send(packet(&f));
                    }
                }
            }
            Some((cap, pcm)) = up_rx.recv() => {
                if cg.may_send(Dir::Up) {
                    let t0 = Instant::now();
                    if let Ok(opus) = up_enc.encode(&pcm) {
                        up_rx_side.rep.encode_us += t0.elapsed().as_micros() as u64;
                        up_rx_side.rep.bytes += opus.len() as u64;
                        up_rx_side.rep.sent += 1;
                        let f = Msg::Frame(Frame { session, dir: Dir::Up, seq: useq, ts: useq.wrapping_mul(wire::FRAME_SAMPLES as u32), cap, opus });
                        useq = useq.wrapping_add(1);
                        let _ = c_out_tx.try_send(packet(&f));
                    }
                }
            }
            Some((hello, s)) = info_rx.recv() => {
                if hello { report.helper_hello = Some(s) } else if report.helper_status.len() < 32 { report.helper_status.push(s) }
            }
            _ = tick.tick() => {
                let _ = down_rx_side.tick(epoch);
                if let Some(pcm) = up_rx_side.tick(epoch) {
                    let cap = epoch.elapsed().as_millis() as u32;
                    let _ = inj_tx.try_send(HelperFrame::Inject { seq: inj_seq, cap, samples: pcm });
                    inj_seq = inj_seq.wrapping_add(1);
                }
            }
        }
    }
    // Let a helper report what it saw before the connection goes.
    while let Ok((hello, s)) = info_rx.try_recv() {
        if hello {
            report.helper_hello = Some(s);
        } else if report.helper_status.len() < 32 {
            report.helper_status.push(s);
        }
    }
    let _ = p_out_tx.try_send(packet(&Msg::Stop {
        session,
        reason: Some("user".into()),
    }));
    pump_p.abort();
    pump_c.abort();
    report.down = down_rx_side.finish();
    report.down.gate_dropped += cg.dropped;
    report.up = up_rx_side.finish();
    report.up.gate_dropped += pg.dropped;
    report.cpu_ms = cpu_ms().saturating_sub(cpu0);
    report.wall_ms = epoch.elapsed().as_millis() as u64;
    Ok(report)
}
