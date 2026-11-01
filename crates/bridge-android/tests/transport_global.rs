//! The process-wide `handle_request` (what the JNI export calls) drives the same transport.
//! One test only: the global instance is shared by everything in this test binary.
use geekbridge_android::handle_request;
use serde_json::{Value, json};

fn call(v: Value) -> Value {
    let out: Value = serde_json::from_str(&handle_request(&v.to_string())).unwrap();
    assert_eq!(out["version"], 1);
    out
}

#[test]
fn global_handler_starts_reports_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let status = call(json!({"version": 1, "action": "transport_status"}));
    assert_eq!(status["result"], json!({"state": "stopped"}));

    let started = call(json!({
        "version": 1, "action": "transport_start",
        "state_dir": dir.path().to_str().unwrap(),
        "device_name": "Global Phone",
        "listen": {"host": "127.0.0.1", "port": 0},
        "allow_loopback": true,
    }));
    assert_eq!(started["ok"], true, "{started}");
    let port = u16::try_from(started["result"]["listen"]["port"].as_u64().unwrap()).unwrap();
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());

    let status = call(json!({"version": 1, "action": "transport_status"}));
    assert_eq!(status["result"]["state"], "running");
    assert_eq!(status["result"]["local"]["device_name"], "Global Phone");

    let stopped = call(json!({"version": 1, "action": "transport_stop"}));
    assert_eq!(stopped["result"]["was_running"], true);
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    let again = call(json!({"version": 1, "action": "transport_stop"}));
    assert_eq!(again["result"]["was_running"], false);
}
