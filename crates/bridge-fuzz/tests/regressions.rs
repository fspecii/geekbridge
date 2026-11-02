// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal repros for every finding the fuzzers produced (see docs/fuzzing.md). Each test runs the
//! fuzz target's own oracle on the reduced input, so it fails for the same reason the fuzzer did.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bridge_fuzz::targets::by_name;

fn run(target: &str, input: &[u8]) {
    (by_name(target).expect("target").run)(input);
}

#[test]
fn filename_cut_to_length_never_ends_in_space_or_dot() {
    // 250 bytes of text, then a space-padded gap: the 200-byte cut used to land after a space.
    run("share", format!("report{}x", " ".repeat(300)).as_bytes());
    run("share", format!("a{}.b", ".".repeat(300)).as_bytes());
}

#[test]
fn filename_cut_to_length_cannot_become_a_windows_device_name() {
    // `CON` + spaces + `x` is not reserved, but the cut-and-trim result `CON` is.
    run("share", format!("CON{}x", " ".repeat(300)).as_bytes());
    let name = bridge_kde::share::sanitize_filename(&format!("CON{}x", " ".repeat(300)));
    assert_eq!(name, "_CON");
}

#[test]
fn udp_discovery_strips_invisible_format_characters_from_names() {
    let id = "0123456789abcdef0123456789abcdef";
    let packet = format!(
        r#"{{"id":1,"type":"kdeconnect.identity","body":{{"deviceId":"{id}","deviceName":"Ev\u202eil\u200d","deviceType":"phone","tcpPort":1716,"protocolVersion":8}}}}"#
    );
    run("udp_discovery", packet.as_bytes());
    let d = bridge_service::discovery::parse_identity(
        packet.as_bytes(),
        "192.168.1.8".parse().unwrap(),
        1,
    )
    .unwrap();
    assert_eq!(d.name, "Evil");
    let only_invisible = packet.replace(r"Ev\u202eil\u200d", r"\u202e\u200b");
    assert!(
        bridge_service::discovery::parse_identity(
            only_invisible.as_bytes(),
            "192.168.1.8".parse().unwrap(),
            1
        )
        .is_err()
    );
}

#[test]
fn repeated_uids_do_not_multiply_a_vcard() {
    let vcard = "BEGIN:VCARD\r\nFN:A\r\nEND:VCARD\r\n".repeat(500);
    let uids = vec!["u1"; 100];
    let body = serde_json::json!({ "uids": uids, "u1": vcard });
    let body = body.as_object().unwrap();
    let parsed = bridge_core::phone_link::parse_vcards(body).unwrap();
    assert_eq!(parsed.len(), 1);
    let line =
        serde_json::json!({ "id": 1, "type": "kdeconnect.contacts.response_vcards", "body": body });
    run("phone_wire", line.to_string().as_bytes());
}

#[test]
fn ufw_rules_with_absurd_ports_and_long_lines_are_handled() {
    run(
        "doctor_ufw",
        b"-A ufw-user-input -p tcp --dport 99999999999999999999:1 -j ACCEPT\n",
    );
    run("doctor_ufw", b"-A ufw-user-input -p tcp --dport -j ACCEPT");
    run("doctor_ufw", b"-A ufw-user-input -p -j ACCEPT");
    let long = format!("-A ufw-user-input {} -j ACCEPT", "-p tcp ".repeat(100_000));
    run("doctor_ufw", long.as_bytes());
}

#[test]
fn debug_packets_with_hostile_text_are_cleaned() {
    run(
        "debug_link",
        "{\"seq\":18446744073709551615,\"lines\":[\"\u{1b}]0;x\u{7}\",\"\u{202e}a\"]}".as_bytes(),
    );
    run("debug_link", br#"{"requestId":"","text":"x"}"#);
    run("hub_debug", b"{\"seq\":1,\"lines\":[]}\n{\"seq\":1,\"lines\":[]}\n{\"requestId\":\"st1\",\"text\":\"a\"}\n");
}

#[test]
fn debug_log_lines_lose_the_deprecated_format_characters() {
    run(
        "debug_link",
        "{\"seq\":1,\"lines\":[\"c\u{206e}d\"]}".as_bytes(),
    );
}
