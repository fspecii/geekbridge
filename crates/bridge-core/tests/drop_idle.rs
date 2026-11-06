// SPDX-License-Identifier: GPL-3.0-or-later
//! `codepages::drop_idle` must never crash the process, whatever else is running.
//!
//! It advises the kernel to forget this process's idle code mappings while other threads execute that very
//! code. A page that is dropped under a running thread must simply fault back in. These tests call it
//! repeatedly and from several threads while others compute, allocate, use the network stack, parse JSON
//! and spawn threads, and check that every result is still right.
use bridge_core::codepages::drop_idle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Work that touches a lot of different code: formatting, hashing, JSON, allocation, sorting.
fn busy_round(seed: u64) -> u64 {
    let mut v: Vec<u64> = (0..2_000)
        .map(|i| (i * 2_654_435_761u64) ^ seed.rotate_left((i % 63) as u32))
        .collect();
    v.sort_unstable();
    let text =
        serde_json::json!({"seed": seed, "list": &v[..16], "s": format!("{seed:x}-{}", v.len())});
    let back: serde_json::Value = serde_json::from_str(&text.to_string()).unwrap();
    assert_eq!(back, text);
    let digest =
        bridge_core::ipc::hmac_sha256(&seed.to_le_bytes(), &[b"x", format!("{}", v[7]).as_bytes()]);
    let mut acc = 0u64;
    for b in digest {
        acc = acc.wrapping_mul(131).wrapping_add(u64::from(b));
    }
    acc ^ v.iter().fold(0, |a, x| a ^ x)
}

fn expected(seed: u64) -> u64 {
    busy_round(seed)
}

#[test]
fn repeated_calls_while_threads_run_never_crash_and_results_stay_right() {
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = (0..4u64)
        .map(|w| {
            let (stop, done) = (stop.clone(), done.clone());
            std::thread::spawn(move || {
                let mut round = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let seed = w * 1_000_003 + round;
                    let want = expected(seed);
                    assert_eq!(busy_round(seed), want, "worker {w} round {round}");
                    done.fetch_add(1, Ordering::Relaxed);
                    round += 1;
                }
            })
        })
        .collect();
    let started = Instant::now();
    let mut calls = 0;
    while calls < 300 || done.load(Ordering::Relaxed) < 50 {
        drop_idle();
        calls += 1;
        // Some calls back to back, some with a short pause so workers run between them.
        if calls % 5 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "workers made no progress"
        );
    }
    stop.store(true, Ordering::Relaxed);
    for w in workers {
        w.join()
            .expect("a worker crashed or computed a wrong result");
    }
}

#[test]
fn several_threads_dropping_at_once_is_harmless() {
    let results: Vec<_> = (0..6)
        .map(|t| {
            std::thread::spawn(move || {
                let mut total = 0usize;
                for i in 0..150 {
                    total = total.wrapping_add(drop_idle());
                    assert_eq!(busy_round(t * 31 + i), expected(t * 31 + i));
                }
                total
            })
        })
        .collect();
    for r in results {
        r.join().expect("a thread crashed");
    }
}

#[test]
fn dropping_while_threads_start_and_finish_and_while_sockets_are_used() {
    let stop = Arc::new(AtomicBool::new(false));
    let dropper = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut n = 0u32;
            while !stop.load(Ordering::Relaxed) {
                drop_idle();
                n += 1;
                std::thread::yield_now();
            }
            n
        })
    };
    // Spawn and join many short threads (new stacks, TLS set-up, thread-exit code paths).
    for round in 0..200u64 {
        let h = std::thread::spawn(move || busy_round(round));
        assert_eq!(h.join().unwrap(), expected(round));
    }
    // Loopback TCP round trips (libc, the kernel interface, DNS-free resolution of an address).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        for _ in 0..50 {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 16];
            let n = s.read(&mut buf).unwrap();
            s.write_all(&buf[..n]).unwrap();
        }
    });
    for i in 0..50u8 {
        use std::io::{Read, Write};
        let mut c = std::net::TcpStream::connect(addr).unwrap();
        c.write_all(&[i; 8]).unwrap();
        let mut back = [0u8; 8];
        c.read_exact(&mut back).unwrap();
        assert_eq!(back, [i; 8]);
    }
    server.join().unwrap();
    stop.store(true, Ordering::Relaxed);
    assert!(dropper.join().expect("the dropping thread crashed") > 0);
}

#[test]
fn unwinding_a_panic_right_after_a_drop_works() {
    // Unwinding runs rarely used code (the unwinder, formatting of the payload): drop right before it.
    for i in 0..50 {
        drop_idle();
        let r = std::panic::catch_unwind(|| {
            drop_idle();
            panic!("expected {i}");
        });
        assert!(r.is_err());
    }
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn it_advises_something_on_linux_and_the_amount_is_plausible() {
    let first = drop_idle();
    assert!(first > 0, "a test binary has file-backed code to drop");
    // Never more than the whole address space we could possibly have mapped as code.
    assert!(first < (1usize << 40), "{first}");
    // Running again right away still works (everything that was dropped is mapped again lazily).
    assert!(drop_idle() > 0);
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
#[test]
fn it_is_a_no_op_elsewhere() {
    assert_eq!(drop_idle(), 0);
}

/// A library unloaded and its address range reused by anonymous memory must never be zeroed by a drop that read
/// `smaps` before the unload: load/unload a small library and map/fill/verify/unmap anonymous pages while others drop.
#[cfg(all(target_os = "linux", target_env = "gnu", target_pointer_width = "64"))]
#[test]
fn unloading_libraries_and_reusing_their_addresses_never_loses_anonymous_data() {
    use std::ffi::{CString, c_void};
    unsafe extern "C" {
        fn dlopen(name: *const std::ffi::c_char, flags: i32) -> *mut c_void;
        fn dlclose(h: *mut c_void) -> i32;
        fn mmap(a: *mut c_void, l: usize, p: i32, f: i32, fd: i32, o: i64) -> *mut c_void;
        fn munmap(a: *mut c_void, l: usize) -> i32;
    }
    let name = CString::new("libresolv.so.2").unwrap();
    // SAFETY: plain loader calls on a system library.
    let probe = unsafe { dlopen(name.as_ptr(), 2) };
    if probe.is_null() {
        return; // not installed here
    }
    unsafe { dlclose(probe) };
    let stop = Arc::new(AtomicBool::new(false));
    let droppers: Vec<_> = (0..2)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    drop_idle();
                }
            })
        })
        .collect();
    let loader = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let name = CString::new("libresolv.so.2").unwrap();
            while !stop.load(Ordering::Relaxed) {
                // SAFETY: load and unload a library that nothing else uses.
                unsafe {
                    let h = dlopen(name.as_ptr(), 2);
                    if !h.is_null() {
                        dlclose(h);
                    }
                }
            }
        })
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        // SAFETY: private anonymous read/write pages of our own, filled, checked and unmapped here.
        unsafe {
            let len = 256 * 1024;
            let p = mmap(std::ptr::null_mut(), len, 3, 0x22, -1, 0).cast::<u8>();
            assert!(!p.is_null() && p as isize != -1);
            std::ptr::write_bytes(p, 0xAB, len);
            std::thread::yield_now();
            let s = std::slice::from_raw_parts(p, len);
            assert!(s.iter().all(|b| *b == 0xAB), "anonymous memory was zeroed");
            munmap(p.cast(), len);
        }
    }
    stop.store(true, Ordering::Relaxed);
    loader.join().unwrap();
    for d in droppers {
        d.join().unwrap();
    }
}
