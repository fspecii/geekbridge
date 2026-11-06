// SPDX-License-Identifier: GPL-3.0-or-later
//! The authenticated control listener (loopback only). Wire contract: `bridge_core::ipc`.
//!
//! Order of operations per connection, each step bounded:
//! hello (with a fresh nonce) -> one request line -> constant-time MAC check -> only then parse and
//! execute -> MAC'd response -> close. A request with a missing or wrong MAC is never parsed and
//! reaches no handler. The whole exchange has one deadline, request and response sizes are capped,
//! and at most [`MAX_CONNECTIONS`] exchanges run at once (extra connections are closed at once).

use crate::{
    daemon::Daemon,
    private::{Unavailable, random_nonce},
};
use bridge_core::ipc::{
    self, ErrorBody, ErrorCode, Hello, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Request, Response,
    Token,
};
use std::{io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};

pub const MAX_CONNECTIONS: usize = 8;
pub const EXCHANGE_DEADLINE: Duration = Duration::from_secs(8);
/// Upper bound for a request that keeps running after its client has given up (see `exchange`).
const WORK_DEADLINE: Duration = Duration::from_secs(90);

pub enum Control {
    Ready {
        daemon: Arc<Daemon>,
        token: Token,
    },
    /// Private state could not be opened: say so and do nothing else.
    Unavailable(Unavailable),
}

pub async fn serve_control(listener: TcpListener, control: Arc<Control>) -> io::Result<()> {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) if is_transient(&err) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Err(err) => return Err(err),
        };
        // Never queue: a flood of idle sockets must not delay a real client's slot.
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let control = control.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = timeout(EXCHANGE_DEADLINE, exchange(stream, &control)).await;
        });
    }
}

fn is_transient(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
    ) || err.raw_os_error().is_some()
}

async fn exchange(mut stream: TcpStream, control: &Control) -> io::Result<()> {
    let (daemon, token) = match control {
        Control::Unavailable(why) => {
            let hello = ipc::format_hello(&Hello::Unavailable {
                code: why.code.to_owned(),
                message: why.message.to_owned(),
            });
            stream.write_all(hello.as_bytes()).await?;
            return stream.shutdown().await;
        }
        Control::Ready { daemon, token } => (daemon, token),
    };
    let nonce = random_nonce();
    stream
        .write_all(
            ipc::format_hello(&Hello::Ready {
                nonce: nonce.clone(),
            })
            .as_bytes(),
        )
        .await?;

    let line = read_request_line(&mut stream).await?;
    // Authenticate before looking at the body at all.
    let authenticated = ipc::split_frame(&line).and_then(|(mac, body)| {
        let expected = token.request_mac(&nonce, body);
        ipc::constant_time_eq(&expected, &mac).then_some((mac, body))
    });
    let Some((request_mac, body)) = authenticated else {
        stream.write_all(b"ERR unauthorized\n").await?;
        return stream.shutdown().await;
    };

    let response = match serde_json::from_slice::<Request>(body) {
        // A request with several durable steps must not be cut in half by the exchange deadline (the
        // client is gone after 8 s, the work is not): it runs as its own task, bounded by `WORK_DEADLINE`.
        Ok(request @ Request::Unpair { .. }) => {
            let daemon = daemon.clone();
            let work = tokio::spawn(async move {
                timeout(WORK_DEADLINE, daemon.handle(request))
                    .await
                    .unwrap_or_else(|_| {
                        Response::Error(ErrorBody {
                            code: ErrorCode::Failed,
                            message: "the change took too long".into(),
                        })
                    })
            });
            work.await.unwrap_or_else(|_| {
                Response::Error(ErrorBody {
                    code: ErrorCode::Failed,
                    message: "the change was interrupted".into(),
                })
            })
        }
        Ok(request) => daemon.handle(request).await,
        Err(_) => Response::Error(ErrorBody {
            code: ErrorCode::BadRequest,
            message: "unrecognised request".into(),
        }),
    };
    let mut json = serde_json::to_vec(&response).map_err(io::Error::other)?;
    if json.len() > MAX_RESPONSE_BYTES {
        json = serde_json::to_vec(&Response::Error(ErrorBody {
            code: ErrorCode::Failed,
            message: "response too large".into(),
        }))
        .map_err(io::Error::other)?;
    }
    let mac = token.response_mac(&nonce, &request_mac, &json);
    let mut frame = ipc::encode_hex(&mac).into_bytes();
    frame.push(b' ');
    frame.extend_from_slice(&json);
    frame.push(b'\n');
    stream.write_all(&frame).await?;
    stream.shutdown().await
}

/// One newline-terminated line of at most `MAX_REQUEST_BYTES` plus the MAC prefix. Anything after
/// the newline is ignored.
async fn read_request_line(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let limit = MAX_REQUEST_BYTES + 66;
    let mut line = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if let Some(end) = chunk[..n].iter().position(|b| *b == b'\n') {
            line.extend_from_slice(&chunk[..end]);
            if line.len() > limit {
                return Err(io::Error::other("request too large"));
            }
            return Ok(line);
        }
        line.extend_from_slice(&chunk[..n]);
        if line.len() > limit {
            return Err(io::Error::other("request too large"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_errors_that_a_busy_machine_produces_are_retried_and_others_are_fatal() {
        for kind in [
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::Interrupted,
        ] {
            assert!(is_transient(&kind.into()), "{kind:?}");
        }
        // EMFILE / ENFILE style failures carry an OS error code and are waited out, not fatal.
        assert!(is_transient(&io::Error::from_raw_os_error(24)));
        assert!(!is_transient(&io::Error::other("listener closed")));
        assert!(!is_transient(&io::ErrorKind::InvalidInput.into()));
    }
}
