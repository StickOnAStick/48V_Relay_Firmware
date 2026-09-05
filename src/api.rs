//! Minimal HTTP API served directly from the Embassy TCP stack.
//!
//! This module has no SPI or GPIO access. It receives HTTP requests through a
//! TCP socket and asks the relay controller to apply validated commands.

use embassy_net::{Stack, tcp::TcpSocket};
use embedded_io_async::Write;

use crate::{board::RELAY_COUNT, relay::RelayControl};

const HTTP_PORT: u16 = 80;
const REQUEST_BUFFER_SIZE: usize = 1024;
const SOCKET_BUFFER_SIZE: usize = 2048;

const INDEX_HTML: &[u8] = br#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Relay Controller</title></head>
<body><h1>Relay Controller</h1><p>Use the HTTP API to control relays.</p></body></html>"#;

/// A TCP/HTTP server worker. Spawn several instances for concurrent clients;
/// each worker gets its own `TcpSocket`, while all share the same `Stack`.
#[embassy_executor::task(pool_size = 4)]
pub async fn http_server_task(stack: Stack<'static>, relays: RelayControl) -> ! {
    let mut rx_buffer = [0_u8; SOCKET_BUFFER_SIZE];
    let mut tx_buffer = [0_u8; SOCKET_BUFFER_SIZE];
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

    loop {
        if socket.accept(HTTP_PORT).await.is_err() {
            continue;
        }

        let mut request = [0_u8; REQUEST_BUFFER_SIZE];
        let request_len = read_headers(&mut socket, &mut request).await;

        let response = match request_len {
            Some(len) => route(&request[..len], relays).await,
            None => Response::BadRequest,
        };

        match response {
            Response::Html => respond(&mut socket, b"200 OK", b"text/html; charset=utf-8", INDEX_HTML).await,
            Response::RelayState => {
                let mut body = [0_u8; 96];
                let body_len = write_relay_state(&mut body, relays.state_mask());
                respond(&mut socket, b"200 OK", b"application/json", &body[..body_len]).await;
            }
            Response::CommandAccepted => {
                respond(&mut socket, b"202 Accepted", b"application/json", b"{\"accepted\":true}").await;
            }
            Response::BadRequest => {
                respond(&mut socket, b"400 Bad Request", b"application/json", b"{\"error\":\"invalid relay request\"}").await;
            }
            Response::NotFound => {
                respond(&mut socket, b"404 Not Found", b"application/json", b"{\"error\":\"not found\"}").await;
            }
        }

        let _ = socket.flush().await;
        socket.close();
    }
}

enum Response {
    Html,
    RelayState,
    CommandAccepted,
    BadRequest,
    NotFound,
}

async fn read_headers(socket: &mut TcpSocket<'_>, buffer: &mut [u8]) -> Option<usize> {
    let mut used = 0;
    while used < buffer.len() {
        let read = socket.read(&mut buffer[used..]).await.ok()?;
        if read == 0 {
            return None;
        }
        used += read;

        if buffer[..used].windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            return Some(used);
        }
    }
    None
}

async fn route(request: &[u8], relays: RelayControl) -> Response {
    let Some(line_end) = request.windows(2).position(|bytes| bytes == b"\r\n") else {
        return Response::BadRequest;
    };
    let line = &request[..line_end];

    if line == b"GET / HTTP/1.1" || line == b"GET / HTTP/1.0" {
        return Response::Html;
    }
    if line == b"GET /api/relays HTTP/1.1" || line == b"GET /api/relays HTTP/1.0" {
        return Response::RelayState;
    }

    let Some(target) = line.strip_prefix(b"POST /api/relays/") else {
        return Response::NotFound;
    };
    let Some((index, suffix)) = parse_relay_target(target) else {
        return Response::BadRequest;
    };
    let on = match suffix {
        b"/on HTTP/1.1" | b"/on HTTP/1.0" => true,
        b"/off HTTP/1.1" | b"/off HTTP/1.0" => false,
        _ => return Response::BadRequest,
    };

    // The command is placed on a bounded queue. The relay task—not this
    // network-facing task—performs the GPIO write.
    if index >= RELAY_COUNT {
        return Response::BadRequest;
    }
    match relays.set(index, on).await {
        Ok(()) => Response::CommandAccepted,
        Err(_) => Response::BadRequest,
    }
}

fn parse_relay_target(target: &[u8]) -> Option<(usize, &[u8])> {
    let digits = target.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }

    let mut index = 0_usize;
    for byte in &target[..digits] {
        index = index.checked_mul(10)?;
        index = index.checked_add((byte - b'0') as usize)?;
    }
    Some((index, &target[digits..]))
}

async fn respond(socket: &mut TcpSocket<'_>, status: &[u8], content_type: &[u8], body: &[u8]) {
    let _ = socket.write_all(b"HTTP/1.1 ").await;
    let _ = socket.write_all(status).await;
    let _ = socket.write_all(b"\r\nContent-Type: ").await;
    let _ = socket.write_all(content_type).await;
    let _ = socket.write_all(b"\r\nConnection: close\r\n\r\n").await;
    let _ = socket.write_all(body).await;
}

fn write_relay_state(buffer: &mut [u8], mask: u32) -> usize {
    let mut used = 0;
    append(buffer, &mut used, b"{\"relays\":[");
    for index in 0..RELAY_COUNT {
        if index != 0 {
            append(buffer, &mut used, b",");
        }
        let state: &[u8] = if mask & (1_u32 << index) != 0 {
            b"true"
        } else {
            b"false"
        };
        append(buffer, &mut used, state);
    }
    append(buffer, &mut used, b"]}");
    used
}

fn append(buffer: &mut [u8], used: &mut usize, bytes: &[u8]) {
    let end = *used + bytes.len();
    buffer[*used..end].copy_from_slice(bytes);
    *used = end;
}
