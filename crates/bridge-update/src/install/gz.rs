// SPDX-License-Identifier: GPL-3.0-or-later
//! A small streaming gzip reader (RFC 1951/1952), so the installer needs no decompression crate.
//!
//! It is pull based (`Read`), keeps only a 32 KiB window plus one block's worth of output in
//! memory, checks the gzip CRC-32 and length trailer, refuses data after the trailer, and stops
//! at an output limit (a decompression bomb ends in an error, not in a full disk).
use std::io::{self, Read};

fn bad(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

const WINDOW: usize = 32 * 1024;
const CHUNK: usize = 64 * 1024;
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Canonical Huffman code in the "count per length + symbols in code order" form.
struct Table {
    count: [u16; 16],
    sym: Vec<u16>,
}

impl Table {
    fn new(lengths: &[u8]) -> io::Result<Table> {
        let mut count = [0_u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        let mut left: i32 = 1;
        for &c in &count[1..] {
            left = (left << 1) - i32::from(c);
            if left < 0 {
                return Err(bad("over-subscribed code"));
            }
        }
        let mut offs = [0_u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + count[l];
        }
        let mut sym = vec![0_u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                sym[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Table { count, sym })
    }
}

enum State {
    Header,
    Block,
    Stored {
        left: usize,
        last: bool,
    },
    Huff {
        lit: Box<Table>,
        dist: Box<Table>,
        last: bool,
    },
    Trailer,
    Done,
}

pub struct Gunzip<R: Read> {
    src: R,
    inbuf: Box<[u8; 4096]>,
    inpos: usize,
    inlen: usize,
    bitbuf: u32,
    bitcnt: u32,
    /// `[history window | not yet handed out]`; `taken` splits the two.
    out: Vec<u8>,
    taken: usize,
    state: State,
    crc: u32,
    crc_table: Box<[u32; 256]>,
    total: u64,
    limit: u64,
}

impl<R: Read> Gunzip<R> {
    /// `limit` is the most uncompressed bytes that will be produced.
    pub fn new(src: R, limit: u64) -> Self {
        let mut t = Box::new([0_u32; 256]);
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *e = c;
        }
        Gunzip {
            src,
            inbuf: Box::new([0; 4096]),
            inpos: 0,
            inlen: 0,
            bitbuf: 0,
            bitcnt: 0,
            out: Vec::new(),
            taken: 0,
            state: State::Header,
            crc: 0xFFFF_FFFF,
            crc_table: t,
            total: 0,
            limit,
        }
    }

    fn byte(&mut self) -> io::Result<u8> {
        if self.inpos == self.inlen {
            self.inlen = self.src.read(&mut self.inbuf[..])?;
            self.inpos = 0;
            if self.inlen == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the archive is truncated",
                ));
            }
        }
        self.inpos += 1;
        Ok(self.inbuf[self.inpos - 1])
    }

    fn bits(&mut self, n: u32) -> io::Result<u32> {
        while self.bitcnt < n {
            let b = u32::from(self.byte()?);
            self.bitbuf |= b << self.bitcnt;
            self.bitcnt += 8;
        }
        let v = self.bitbuf & ((1_u32 << n) - 1);
        self.bitbuf = self.bitbuf.checked_shr(n).unwrap_or(0);
        self.bitcnt -= n;
        Ok(v)
    }

    fn le(&mut self, n: usize) -> io::Result<u32> {
        let mut v = 0_u32;
        for i in 0..n {
            v |= u32::from(self.byte()?) << (8 * i);
        }
        Ok(v)
    }

    fn skip_zstr(&mut self) -> io::Result<()> {
        for _ in 0..4096 {
            if self.byte()? == 0 {
                return Ok(());
            }
        }
        Err(bad("gzip header field too long"))
    }

    fn decode(&mut self, t: &Table) -> io::Result<usize> {
        let (mut code, mut first, mut index) = (0_i32, 0_i32, 0_i32);
        for len in 1..16 {
            code |= self.bits(1)? as i32;
            let count = i32::from(t.count[len]);
            if code - count < first {
                return Ok(t.sym[(index + (code - first)) as usize] as usize);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(bad("invalid Huffman code"))
    }

    fn header(&mut self) -> io::Result<()> {
        if self.le(2)? != 0x8b1f || self.byte()? != 8 {
            return Err(bad("not a gzip file"));
        }
        let flags = self.byte()?;
        if flags & 0xE0 != 0 {
            return Err(bad("unsupported gzip flags"));
        }
        self.le(4)?; // mtime
        self.le(2)?; // xfl, os
        if flags & 4 != 0 {
            let n = self.le(2)?;
            for _ in 0..n {
                self.byte()?;
            }
        }
        if flags & 8 != 0 {
            self.skip_zstr()?;
        }
        if flags & 16 != 0 {
            self.skip_zstr()?;
        }
        if flags & 2 != 0 {
            self.le(2)?;
        }
        Ok(())
    }

    fn dynamic(&mut self) -> io::Result<(Table, Table)> {
        let nlen = self.bits(5)? as usize + 257;
        let ndist = self.bits(5)? as usize + 1;
        let ncode = self.bits(4)? as usize + 4;
        if nlen > 286 || ndist > 30 {
            return Err(bad("bad dynamic block"));
        }
        let mut lengths = [0_u8; 320];
        for &i in &CL_ORDER[..ncode] {
            lengths[i] = self.bits(3)? as u8;
        }
        let cl = Table::new(&lengths[..19])?;
        let mut lengths = [0_u8; 320];
        let mut i = 0;
        while i < nlen + ndist {
            let sym = self.decode(&cl)?;
            if sym < 16 {
                lengths[i] = sym as u8;
                i += 1;
                continue;
            }
            let (value, rep) = match sym {
                16 => {
                    if i == 0 {
                        return Err(bad("bad repeat"));
                    }
                    (lengths[i - 1], 3 + self.bits(2)? as usize)
                }
                17 => (0, 3 + self.bits(3)? as usize),
                _ => (0, 11 + self.bits(7)? as usize),
            };
            if i + rep > nlen + ndist {
                return Err(bad("bad repeat"));
            }
            lengths[i..i + rep].fill(value);
            i += rep;
        }
        if lengths[256] == 0 {
            return Err(bad("no end-of-block code"));
        }
        Ok((
            Table::new(&lengths[..nlen])?,
            Table::new(&lengths[nlen..nlen + ndist])?,
        ))
    }

    fn fixed() -> io::Result<(Table, Table)> {
        let mut l = [8_u8; 288];
        l[144..256].fill(9);
        l[256..280].fill(7);
        Ok((Table::new(&l)?, Table::new(&[5_u8; 30])?))
    }

    /// Produce more output (or finish). Called only when everything produced so far was handed out.
    fn step(&mut self) -> io::Result<()> {
        if self.taken >= 2 * WINDOW {
            self.out.drain(..self.taken - WINDOW);
            self.taken = WINDOW;
        }
        match std::mem::replace(&mut self.state, State::Done) {
            State::Header => {
                self.header()?;
                self.state = State::Block;
            }
            State::Block => {
                let last = self.bits(1)? == 1;
                match self.bits(2)? {
                    0 => {
                        self.bitbuf = 0;
                        self.bitcnt = 0;
                        let len = self.le(2)?;
                        if self.le(2)? != !len & 0xFFFF {
                            return Err(bad("bad stored block"));
                        }
                        self.state = State::Stored {
                            left: len as usize,
                            last,
                        };
                    }
                    t @ (1 | 2) => {
                        let (lit, dist) = if t == 1 {
                            Self::fixed()?
                        } else {
                            self.dynamic()?
                        };
                        self.state = State::Huff {
                            lit: Box::new(lit),
                            dist: Box::new(dist),
                            last,
                        };
                    }
                    _ => return Err(bad("bad block type")),
                }
            }
            State::Stored { left, last } => {
                for _ in 0..left.min(CHUNK) {
                    let b = self.byte()?;
                    self.out.push(b);
                }
                self.state = match (left > CHUNK, last) {
                    (true, _) => State::Stored {
                        left: left - CHUNK,
                        last,
                    },
                    (false, true) => State::Trailer,
                    (false, false) => State::Block,
                };
            }
            State::Huff { lit, dist, last } => {
                let stop = self.out.len() + CHUNK;
                while self.out.len() < stop {
                    let sym = self.decode(&lit)?;
                    if sym < 256 {
                        self.out.push(sym as u8);
                    } else if sym == 256 {
                        self.state = if last { State::Trailer } else { State::Block };
                        return Ok(());
                    } else {
                        let s = sym - 257;
                        if s >= 29 {
                            return Err(bad("bad length code"));
                        }
                        let len =
                            LEN_BASE[s] as usize + self.bits(u32::from(LEN_EXTRA[s]))? as usize;
                        let d = self.decode(&dist)?;
                        if d >= 30 {
                            return Err(bad("bad distance code"));
                        }
                        let dist_v =
                            DIST_BASE[d] as usize + self.bits(u32::from(DIST_EXTRA[d]))? as usize;
                        if dist_v > self.out.len() {
                            return Err(bad("distance too far back"));
                        }
                        let from = self.out.len() - dist_v;
                        for k in 0..len {
                            let b = self.out[from + k];
                            self.out.push(b);
                        }
                    }
                }
                self.state = State::Huff { lit, dist, last };
            }
            State::Trailer => {
                self.bitbuf = 0;
                self.bitcnt = 0;
                let crc = self.le(4)?;
                let size = self.le(4)?;
                if crc != !self.crc || u64::from(size) != self.total & 0xFFFF_FFFF {
                    return Err(bad("the archive is corrupt (gzip checksum)"));
                }
                if self.inpos < self.inlen || self.src.read(&mut self.inbuf[..1])? != 0 {
                    return Err(bad("data after the end of the gzip stream"));
                }
            }
            State::Done => {}
        }
        Ok(())
    }
}

impl<R: Read> Read for Gunzip<R> {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        if dst.is_empty() {
            return Ok(0);
        }
        loop {
            if self.taken < self.out.len() {
                let n = dst.len().min(self.out.len() - self.taken);
                let chunk = &self.out[self.taken..self.taken + n];
                if self.total + n as u64 > self.limit {
                    return Err(bad("the archive unpacks to more than the allowed size"));
                }
                for &b in chunk {
                    self.crc = self.crc_table[((self.crc ^ u32::from(b)) & 0xFF) as usize]
                        ^ (self.crc >> 8);
                }
                dst[..n].copy_from_slice(chunk);
                self.taken += n;
                self.total += n as u64;
                return Ok(n);
            }
            if matches!(self.state, State::Done) {
                return Ok(0);
            }
            self.step()?;
        }
    }
}
