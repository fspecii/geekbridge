// SPDX-License-Identifier: GPL-3.0-or-later
//! 100 threads x 20 texts on the phone: the list carries the newest of each, and `refresh_messages` per thread (what
//! opening a conversation does) brings the other 19. Nothing is fetched until a thread is asked for.
use bridge_kde::sms;
use bridge_service::phone_data::{Caps, Out, PhoneData};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

const ID: &str = "hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh";
const THREADS: i64 = 100;
const EACH: i64 = 20;

fn caps() -> Caps {
    Caps {
        contacts: false,
        calendar: false,
        calendar_write: false,
        sms_read: true,
        sms_send: false,
        sms_status: false,
        battery_request: false,
        notification_request: false,
        notification_action: false,
        photos: false,
    }
}
fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}
fn drain(rx: &mut mpsc::Receiver<Out>) -> Vec<Out> {
    let mut v = Vec::new();
    while let Ok(o) = rx.try_recv() {
        v.push(o);
    }
    v
}
fn msg(t: i64, k: i64) -> Value {
    json!({"addresses": [{"address": format!("+4670{:07}", 1_000_000 + t)}], "body": format!("t{t} m{k}"),
           "date": 1_793_000_000_000_i64 + t * 1_000_000 + k * 1000, "type": 1, "read": 1,
           "thread_id": t, "_id": t * 100 + k, "sub_id": 1})
}
/// The phone's answer to one `request_conversation`: date <= start, newest first, at most numberToRequest.
fn answer(body: &Map<String, Value>) -> Map<String, Value> {
    let t = body["threadID"].as_i64().unwrap();
    let start = body
        .get("rangeStartTimestamp")
        .and_then(Value::as_i64)
        .unwrap_or(i64::MAX);
    let n = body["numberToRequest"].as_u64().unwrap() as usize;
    let mut v: Vec<Value> = (0..EACH)
        .map(|k| msg(t, k))
        .filter(|m| m["date"].as_i64().unwrap() <= start)
        .collect();
    v.reverse();
    v.truncate(n);
    obj(json!({"version": 2, "messages": v}))
}

#[test]
fn asking_for_each_thread_brings_all_of_its_history() {
    let h = PhoneData::new();
    let (mut rx, _) = h.link_up(ID, "Pixel", caps(), (false, false, true));
    drain(&mut rx);
    let newest: Vec<Value> = (1..=THREADS).map(|t| msg(t, EACH - 1)).collect();
    h.on_packet(
        ID,
        sms::MESSAGES,
        &obj(json!({"version": 2, "messages": newest})),
    );
    let held = |h: &PhoneData| -> usize {
        h.messages_view(None, None, None)
            .unwrap()
            .conversations
            .iter()
            .map(|c| c.message_count)
            .sum()
    };
    assert_eq!(
        held(&h),
        THREADS as usize,
        "the list alone is one message per thread"
    );
    assert!(
        drain(&mut rx).is_empty(),
        "nothing asks for history until a thread is opened"
    );
    for t in 1..=THREADS {
        h.refresh_messages(ID, Some(t)).unwrap();
        for o in drain(&mut rx) {
            assert_eq!(o.kind, sms::REQUEST_CONVERSATION);
            h.on_packet(ID, sms::MESSAGES, &answer(&o.body));
        }
    }
    assert_eq!(held(&h), (THREADS * EACH) as usize, "all 2,000 crossed");
}
