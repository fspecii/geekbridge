// SPDX-License-Identifier: GPL-3.0-or-later
//! PCM in and out on the computer (EXPERIMENTAL): the far party to the chosen headset, the headset microphone
//! into the call. Linux uses PipeWire through `pw-cat` in raw mode (16 kHz mono s16, 20 ms node latency, media
//! role "Communication" so WirePlumber's policy treats it like a call). The target is a node name or serial from
//! the headset routing already chosen in `bridge-calls` (or `None` for the default device).
//!
//! Nothing here writes audio to disk. The `pw-cat` processes read and write pipes only.
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::wire::{FRAME_SAMPLES, RATE};

pub trait PcmSink: Send {
    fn write(&mut self, pcm: &[i16]) -> io::Result<()>;
}

pub trait PcmSource: Send {
    /// Blocks until exactly one frame ([`FRAME_SAMPLES`]) is read.
    fn read(&mut self, pcm: &mut [i16]) -> io::Result<()>;
}

/// Environment for a PipeWire client: lets tests point `pw-cat` at a private daemon (`XDG_RUNTIME_DIR` plus
/// `PIPEWIRE_REMOTE`) instead of the user's session.
#[derive(Clone, Debug, Default)]
pub struct PwEnv {
    pub runtime_dir: Option<String>,
    pub remote: Option<String>,
}

fn pw_cat(mode: &str, target: Option<&str>, env: &PwEnv) -> Command {
    let mut c = Command::new("pw-cat");
    c.arg(mode)
        .args([
            "--raw",
            "--format",
            "s16",
            "--channels",
            "1",
            "--latency",
            "20ms",
        ])
        .args(["--rate", &RATE.to_string()])
        .args([
            "--media-category",
            if mode == "--playback" {
                "Playback"
            } else {
                "Capture"
            },
        ])
        .args(["--media-role", "Communication"])
        .args([
            "-P",
            "node.name=geekbridge-callaudio,node.description=GeekBridge phone call (experimental)",
        ]);
    if let Some(t) = target {
        c.args(["--target", t]);
    }
    if let Some(d) = &env.runtime_dir {
        c.env("XDG_RUNTIME_DIR", d);
    }
    if let Some(r) = &env.remote {
        c.env("PIPEWIRE_REMOTE", r);
    }
    c.stderr(Stdio::null());
    c
}

pub struct PwSink {
    child: Child,
    stdin: ChildStdin,
    buf: Vec<u8>,
}

impl PwSink {
    pub fn spawn(target: Option<&str>, env: &PwEnv) -> io::Result<Self> {
        let mut child = pw_cat("--playback", target, env)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped");
        Ok(Self {
            child,
            stdin,
            buf: Vec::with_capacity(FRAME_SAMPLES * 2),
        })
    }
}

impl PcmSink for PwSink {
    fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        self.buf.clear();
        self.buf.extend(pcm.iter().flat_map(|s| s.to_le_bytes()));
        self.stdin.write_all(&self.buf)
    }
}

impl Drop for PwSink {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct PwSource {
    child: Child,
    stdout: ChildStdout,
    buf: Vec<u8>,
}

impl PwSource {
    pub fn spawn(target: Option<&str>, env: &PwEnv) -> io::Result<Self> {
        let mut child = pw_cat("--record", target, env)
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped");
        Ok(Self {
            child,
            stdout,
            buf: vec![0; FRAME_SAMPLES * 2],
        })
    }
}

impl PcmSource for PwSource {
    fn read(&mut self, pcm: &mut [i16]) -> io::Result<()> {
        let n = pcm.len() * 2;
        self.buf.resize(n, 0);
        self.stdout.read_exact(&mut self.buf[..n])?;
        for (o, c) in pcm.iter_mut().zip(self.buf[..n].as_chunks::<2>().0) {
            *o = i16::from_le_bytes(*c);
        }
        Ok(())
    }
}

impl Drop for PwSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Test sink: keeps only running analysis numbers, never the samples.
#[derive(Default)]
pub struct MeterSink {
    pub frames: u64,
    pub window: Vec<i16>,
    pub window_cap: usize,
}

impl MeterSink {
    /// Keeps at most the last `window_cap` samples in memory (for a frequency check), nothing on disk.
    pub fn new(window_cap: usize) -> Self {
        Self {
            frames: 0,
            window: Vec::new(),
            window_cap,
        }
    }
}

impl PcmSink for MeterSink {
    fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        self.frames += 1;
        self.window.extend_from_slice(pcm);
        if self.window.len() > self.window_cap {
            let cut = self.window.len() - self.window_cap;
            self.window.drain(..cut);
        }
        Ok(())
    }
}
