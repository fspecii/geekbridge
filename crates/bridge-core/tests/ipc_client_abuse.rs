// SPDX-License-Identifier: GPL-3.0-or-later
//! The control client against hostile or broken "services" on the loopback port: something that is not
//! GeekBridge, a service with another secret, truncated and oversize frames, and a reply of the wrong shape.
//! Each must end in a typed error and never in a hang, a panic or an accepted answer.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use bridge_core::ipc::{
    self, Hello, IpcError, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Request, Response, Token,
    encode_hex, format_hello,
};

const NONCE: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn token(b: u8) -> Token {
    Token::from_bytes([b; 32])
}

/// A one-connection fake service: sends `greeting`, reads the client's request line, then runs `then`.
fn serve(
    greeting: Vec<u8>,
    then: impl FnOnce(&mut TcpStream, Vec<u8>) + Send + 'static,
) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.write_all(&greeting).unwrap();
        let mut line = Vec::new();
        let mut b = [0_u8; 1];
        while s.read(&mut b).map(|n| n == 1).unwrap_or(false) && b[0] != b'\n' {
            line.push(b[0]);
        }
        then(&mut s, line);
    });
    (port, handle)
}

fn ready() -> Vec<u8> {
    format_hello(&Hello::Ready {
        nonce: NONCE.into(),
    })
    .into_bytes()
}

/// `<mac> <json>\n` where the mac is the real response mac for `request_line` under `key`.
fn signed_reply(key: &Token, request_line: &[u8], body: &[u8]) -> Vec<u8> {
    let request_mac: [u8; 32] =
        ipc::decode_hex::<32>(std::str::from_utf8(&request_line[..64]).unwrap()).unwrap();
    let mac = key.response_mac(NONCE, &request_mac, body);
    let mut out = encode_hex(&mac).into_bytes();
    out.push(b' ');
    out.extend_from_slice(body);
    out.push(b'\n');
    out
}

fn status_call(port: u16, t: &Token) -> Result<Response, IpcError> {
    ipc::call(port, t, &Request::Status)
}

#[test]
fn a_service_that_rejects_the_token_is_reported_as_unauthorized() {
    let (port, h) = serve(ready(), |s, _| {
        s.write_all(b"ERR unauthorized\n").unwrap();
    });
    let err = status_call(port, &token(1)).unwrap_err();
    assert!(matches!(err, IpcError::Unauthorized), "{err}");
    assert!(err.to_string().contains("rejected the control token"));
    h.join().unwrap();
}

#[test]
fn a_reply_signed_with_another_secret_is_refused() {
    let body = serde_json::to_vec(&Response::Ack).unwrap();
    let (port, h) = serve(ready(), move |s, line| {
        // A fake that does not know the client's token signs with its own.
        let reply = signed_reply(&token(9), &line, &body);
        s.write_all(&reply).unwrap();
    });
    let err = status_call(port, &token(1)).unwrap_err();
    assert!(
        matches!(&err, IpcError::Protocol(m) if m.contains("authentication failed")),
        "{err}"
    );
    h.join().unwrap();
}

#[test]
fn a_correctly_signed_reply_of_the_wrong_kind_is_not_accepted_as_a_status() {
    let body = serde_json::to_vec(&Response::Ack).unwrap();
    let t = token(2);
    let (port, h) = serve(ready(), {
        let t = t.clone();
        move |s, line| s.write_all(&signed_reply(&t, &line, &body)).unwrap()
    });
    let err = ipc::status(port, &t).unwrap_err();
    assert!(
        matches!(&err, IpcError::Protocol(m) if m.contains("unexpected reply")),
        "{err}"
    );
    h.join().unwrap();
    let body = serde_json::to_vec(&Response::Ack).unwrap();
    let (port, h) = serve(ready(), {
        let t = t.clone();
        move |s, line| s.write_all(&signed_reply(&t, &line, &body)).unwrap()
    });
    let err = ipc::events(port, &t, 0).unwrap_err();
    assert!(
        matches!(&err, IpcError::Protocol(m) if m.contains("unexpected reply")),
        "{err}"
    );
    h.join().unwrap();
}

#[test]
fn malformed_truncated_and_oversize_replies_are_protocol_or_io_errors() {
    let t = token(3);
    // Not a frame at all.
    let (port, h) = serve(ready(), |s, _| s.write_all(b"hello there\n").unwrap());
    assert!(
        matches!(status_call(port, &t).unwrap_err(), IpcError::Protocol(m) if m.contains("malformed response frame"))
    );
    h.join().unwrap();

    // The service hangs up right after the request: a truncated exchange, not a hang.
    let (port, h) = serve(ready(), |_s, _| {});
    assert!(matches!(
        status_call(port, &t).unwrap_err(),
        IpcError::Io(_)
    ));
    h.join().unwrap();

    // A frame whose body is one byte over the response cap, even with a plausible mac field.
    let (port, h) = serve(ready(), |s, _| {
        let mut frame = vec![b'a'; 64];
        frame.push(b' ');
        frame.extend(std::iter::repeat_n(b'x', MAX_RESPONSE_BYTES + 1));
        frame.push(b'\n');
        let _ = s.write_all(&frame);
    });
    assert!(
        matches!(status_call(port, &t).unwrap_err(), IpcError::Protocol(m) if m.contains("response too large"))
    );
    h.join().unwrap();

    // A line that never ends is cut off at the cap rather than buffered forever.
    let (port, h) = serve(ready(), |s, _| {
        let chunk = vec![b'y'; 64 * 1024];
        for _ in 0..8 {
            if s.write_all(&chunk).is_err() {
                break;
            }
        }
    });
    assert!(matches!(
        status_call(port, &t).unwrap_err(),
        IpcError::Io(_)
    ));
    h.join().unwrap();

    // Valid mac, body that is not a Response.
    let t2 = token(4);
    let (port, h) = serve(ready(), {
        let t2 = t2.clone();
        move |s, line| {
            s.write_all(&signed_reply(&t2, &line, b"{\"kind\":\"nope\"}"))
                .unwrap()
        }
    });
    assert!(matches!(
        status_call(port, &t2).unwrap_err(),
        IpcError::Protocol(_)
    ));
    h.join().unwrap();
}

#[test]
fn hostile_greetings_are_refused_before_a_request_is_sent() {
    let t = token(5);
    // Not our protocol.
    let (port, h) = serve(b"SSH-2.0-OpenSSH\n".to_vec(), |_s, _| {});
    assert!(
        matches!(status_call(port, &t).unwrap_err(), IpcError::Protocol(m) if m.contains("not a GeekBridge"))
    );
    let _ = h.join();
    // Short or non-hex nonce.
    let (port, h) = serve(b"GB-IPC 1 abcd\n".to_vec(), |_s, _| {});
    assert!(
        matches!(status_call(port, &t).unwrap_err(), IpcError::Protocol(m) if m.contains("nonce"))
    );
    let _ = h.join();
    // A greeting longer than the 256-byte cap.
    let mut long = b"GB-IPC 1 ".to_vec();
    long.extend(std::iter::repeat_n(b'a', 4096));
    long.push(b'\n');
    let (port, h) = serve(long, |_s, _| {});
    assert!(status_call(port, &t).is_err());
    let _ = h.join();
    // The service says it could not open its state: reported, not retried with the token.
    let (port, h) = serve(
        format_hello(&Hello::Unavailable {
            code: "state_locked".into(),
            message: "another instance".into(),
        })
        .into_bytes(),
        |_s, _| {},
    );
    assert!(
        matches!(status_call(port, &t).unwrap_err(), IpcError::Unavailable { code, .. } if code == "state_locked")
    );
    let _ = h.join();
}

#[test]
fn an_oversize_request_is_refused_locally_and_never_sent() {
    let t = token(6);
    let (port, h) = serve(ready(), |s, line| {
        // The client must close without sending a request line.
        assert!(line.is_empty(), "a request was sent");
        let _ = s.flush();
    });
    let big = Request::Connect {
        device_id: "d".repeat(MAX_REQUEST_BYTES + 1),
        endpoint: None,
    };
    let err = ipc::call(port, &t, &big).unwrap_err();
    assert!(
        matches!(&err, IpcError::Protocol(m) if m.contains("request too large")),
        "{err}"
    );
    h.join().unwrap();
}

#[test]
fn request_and_response_macs_bind_the_nonce_and_each_other() {
    let t = token(7);
    let body = br#"{"kind":"ack"}"#;
    let req = t.request_mac(NONCE, body);
    assert_ne!(
        req,
        t.request_mac(&NONCE.replace('0', "1"), body),
        "nonce must change the mac"
    );
    assert_ne!(req, t.request_mac(NONCE, b"{\"kind\":\"status\"}"));
    let resp = t.response_mac(NONCE, &req, body);
    assert_ne!(
        resp,
        t.response_mac(NONCE, &t.request_mac(NONCE, b"other"), body),
        "response is bound to its request"
    );
    assert_ne!(req, resp);
}
