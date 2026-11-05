// SPDX-License-Identifier: GPL-3.0-or-later
//! Unread counts end to end against a stand-in phone: what the phone reports in its conversation
//! list (an unread map listing only threads with unread texts) drives the number the tray shows,
//! and reading on the computer lowers it again.

use bridge_kde::sms;
use bridge_service::{
    phone_data::{Caps, Disposition, Out, PhoneData},
    tray::model::TrayInput,
};
use serde_json::{Map, Value, json};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const ID: &str = "pppppppppppppppppppppppppppppppp";

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn msg(thread: i64, id: i64, body: &str, date: u64) -> Value {
    json!({"addresses": [{"address": format!("+4670{:07}", 1_000_000 + thread)}], "body": body,
           "date": date, "type": 1, "read": 0, "thread_id": thread, "_id": id, "sub_id": 1})
}
fn phone() -> (std::sync::Arc<PhoneData>, mpsc::Receiver<Out>) {
    let h = PhoneData::new();
    let caps = Caps {
        sms_read: true,
        sms_send: true,
        ..Caps::default()
    };
    let (rx, _) = h.link_up(ID, "Pixel", caps, (false, false, true));
    (h, rx)
}
fn tray_line(h: &PhoneData) -> String {
    let input = TrayInput {
        connected: vec!["Pixel".into()],
        paired: 1,
        unread: h.unread_total(),
        ..TrayInput::default()
    };
    input.tooltip().1
}
fn list(h: &PhoneData, unread: Value, messages: Vec<Value>) {
    let p = obj(json!({"version": 2, "unread": unread, "messages": messages}));
    assert_eq!(h.on_packet(ID, sms::MESSAGES, &p), Disposition::Accepted);
}

#[test]
fn reading_on_the_phone_lowers_the_count_and_the_tray_line() {
    let (h, _rx) = phone();
    let now = now_ms();
    list(
        &h,
        json!({"4": 3, "5": 1}),
        vec![
            msg(4, 1, "a", now - 5000),
            msg(4, 2, "b", now - 4000),
            msg(4, 3, "c", now - 3000),
            msg(5, 4, "d", now - 2000),
        ],
    );
    assert_eq!(h.unread_total(), 4);
    assert!(
        tray_line(&h).ends_with("4 unread messages"),
        "{}",
        tray_line(&h)
    );
    // Thread 4 was read on the phone: it is absent from the map, and only its newest row is re-sent.
    list(&h, json!({"5": 1}), vec![msg(5, 4, "d", now - 2000)]);
    assert_eq!(h.unread_total(), 1);
    assert!(
        tray_line(&h).ends_with("1 unread message"),
        "{}",
        tray_line(&h)
    );
    // Everything read: the map is present but empty, and the tray says nothing about unread.
    list(&h, json!({}), vec![msg(5, 4, "d", now - 2000)]);
    assert_eq!(h.unread_total(), 0);
    assert!(!tray_line(&h).contains("unread"), "{}", tray_line(&h));
    // A new text afterwards counts again.
    let p = obj(json!({"version": 2, "messages": [msg(4, 9, "new", now)]}));
    h.on_packet(ID, sms::MESSAGES, &p);
    assert_eq!(h.unread_total(), 1);
}

#[test]
fn reading_in_the_computer_window_lowers_the_count_and_a_later_phone_report_agrees() {
    let (h, _rx) = phone();
    let now = now_ms();
    list(
        &h,
        json!({"4": 2, "5": 1}),
        vec![
            msg(4, 1, "a", now - 5000),
            msg(4, 2, "b", now - 4000),
            msg(5, 3, "c", now - 3000),
        ],
    );
    assert_eq!(h.unread_total(), 3);
    h.mark_read(ID, 4).unwrap();
    assert_eq!(h.unread_total(), 1);
    // The phone has not seen the read yet: the same report must not bring the count back.
    list(
        &h,
        json!({"4": 2, "5": 1}),
        vec![msg(5, 3, "c", now - 3000)],
    );
    assert_eq!(h.unread_total(), 1);
    // Then the phone reads thread 5 itself.
    list(&h, json!({}), vec![msg(5, 3, "c", now - 3000)]);
    assert_eq!(h.unread_total(), 0);
    assert!(!tray_line(&h).contains("unread"));
}
