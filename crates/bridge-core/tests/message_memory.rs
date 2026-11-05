//! Memory regression for the message store: a deterministic accounting of the heap a synthetic
//! store of 10,000 typical messages holds, counted by a global allocator (not process RSS).
//! Each live block is charged as glibc would charge it (16-byte granule, 8-byte header, 32 minimum).

use bridge_core::messages::{Conversation, Message, WireMessage};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

struct Counting;
static REQUESTED: AtomicIsize = AtomicIsize::new(0);
static CHARGED: AtomicIsize = AtomicIsize::new(0);
static BLOCKS: AtomicIsize = AtomicIsize::new(0);

fn charge(size: usize) -> isize {
    ((size + 8 + 15) & !15).max(32) as isize
}
// SAFETY: forwards every call to the system allocator and only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        REQUESTED.fetch_add(l.size() as isize, Ordering::Relaxed);
        CHARGED.fetch_add(charge(l.size()), Ordering::Relaxed);
        BLOCKS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        REQUESTED.fetch_sub(l.size() as isize, Ordering::Relaxed);
        CHARGED.fetch_sub(charge(l.size()), Ordering::Relaxed);
        BLOCKS.fetch_sub(1, Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        REQUESTED.fetch_add(new as isize - l.size() as isize, Ordering::Relaxed);
        CHARGED.fetch_add(charge(new) - charge(l.size()), Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}
#[global_allocator]
static A: Counting = Counting;

const MESSAGES: usize = 10_000;
const THREADS: usize = 50;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn wire(i: usize, rng: &mut Lcg) -> serde_json::Value {
    let thread = (i % THREADS) as i64 + 1;
    let len = 20 + (rng.next() % 141) as usize;
    let body: String = (0..len)
        .map(|k| {
            if k % 7 == 6 {
                ' '
            } else {
                (b'a' + (rng.next() % 26) as u8) as char
            }
        })
        .collect();
    let group = i % 20 == 7;
    let mms = i % 33 == 5;
    let mut addrs = vec![serde_json::json!({"address": format!("+1555010{:04}", thread)})];
    if group {
        for n in 1..3 {
            addrs.push(serde_json::json!({"address": format!("+1555020{:04}", thread * 10 + n)}));
        }
    }
    let mut m = serde_json::json!({
        "addresses": addrs, "body": body, "date": 1_700_000_000_000u64 + i as u64 * 60_000,
        "type": if i.is_multiple_of(3) { 2 } else { 1 }, "read": i32::from(!i.is_multiple_of(10)),
        "thread_id": thread, "_id": i, "sub_id": 1,
    });
    if mms {
        m["attachments"] = serde_json::json!([{"part_id": 1}, {"part_id": 2}]);
    }
    m
}

fn message(i: usize, rng: &mut Lcg) -> Message {
    let w: WireMessage = serde_json::from_value(wire(i, rng)).unwrap();
    Message::from_wire(w).unwrap()
}

#[test]
fn bytes_per_stored_message_stay_under_the_budget() {
    let before = (
        REQUESTED.load(Ordering::SeqCst),
        CHARGED.load(Ordering::SeqCst),
        BLOCKS.load(Ordering::SeqCst),
    );
    let mut convs: Vec<Conversation> = (1..=THREADS as i64)
        .map(|t| Conversation::new(bridge_core::messages::ThreadId(t)))
        .collect();
    let mut body_bytes = 0usize;
    let mut rng = Lcg(42);
    for i in 0..MESSAGES {
        let m = message(i, &mut rng);
        body_bytes += m.body.len();
        let c = &mut convs[(m.key.thread_id.0 - 1) as usize];
        c.merge(m).unwrap();
    }
    let req = (REQUESTED.load(Ordering::SeqCst) - before.0) as f64;
    let chg = (CHARGED.load(Ordering::SeqCst) - before.1) as f64;
    let blocks = (BLOCKS.load(Ordering::SeqCst) - before.2) as f64;
    let n = MESSAGES as f64;
    println!(
        "MSGMEM store of {MESSAGES}: requested {:.1} B/msg, glibc-charged {:.1} B/msg ({:.0} kB total), {:.2} blocks/msg; body text {:.1} B/msg (floor)",
        req / n,
        chg / n,
        chg / 1000.0,
        blocks / n,
        body_bytes as f64 / n
    );
    assert_eq!(convs.iter().map(Conversation::len).sum::<usize>(), MESSAGES);
    let limit = BUDGET_CHARGED_BYTES_PER_MESSAGE;
    assert!(
        chg / n <= limit,
        "{:.1} B/msg over the {limit} budget",
        chg / n
    );
}

const BUDGET_CHARGED_BYTES_PER_MESSAGE: f64 = 220.0;

#[test]
fn a_thread_shares_one_recipient_list_and_a_huge_body_is_cut_to_the_cap() {
    use bridge_core::messages::{MAX_STORED_BODY_BYTES, ThreadId};
    use std::sync::Arc;
    let mut rng = Lcg(1);
    let mut c = Conversation::new(ThreadId(1));
    for i in [0usize, 50, 100] {
        c.merge(message(i, &mut rng)).unwrap();
    }
    let lists: Vec<_> = c.iter().map(|m| Arc::as_ptr(&m.addresses)).collect();
    assert!(
        lists.windows(2).all(|w| std::ptr::addr_eq(w[0], w[1])),
        "one shared list"
    );

    let mut big = message(150, &mut rng);
    big.body = "\u{20ac}".repeat(MAX_STORED_BODY_BYTES).into_boxed_str();
    let key = big.key;
    c.merge(big.clone()).unwrap();
    let stored = &c.get(&key).unwrap().body;
    assert!(stored.len() <= MAX_STORED_BODY_BYTES && stored.ends_with('\u{2026}'));
    // The same message again is still a duplicate, not an update every time.
    assert_eq!(
        c.merge(big).unwrap(),
        bridge_core::messages::MergeOutcome::Duplicate
    );
}
