//! Minimal HTTP API served directly from the Embassy TCP stack.
//!
//! This module has no SPI or GPIO access. It receives HTTP requests through a
//! TCP socket and asks the relay controller to apply validated commands.

use embassy_net::{Stack, tcp::TcpSocket};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_io_async::Write;
use log::{info, warn};

use crate::{board::RELAY_COUNT, tasks::relay::RelayControl};

const HTTP_PORT: u16 = 80;
const REQUEST_BUFFER_SIZE: usize = 1024;
const SOCKET_BUFFER_SIZE: usize = 2048;

// HTTP is a byte protocol. These delimiters make the parser below describe
// the protocol instead of hiding it behind `windows(2)` and `windows(4)`.
const CRLF: &[u8] = b"\r\n";
const END_OF_HTTP_HEADERS: &[u8] = b"\r\n\r\n";
const HTTP_FIELD_SEPARATOR: u8 = b' ';
const HTTP_GET: &[u8] = b"GET";
const HTTP_POST: &[u8] = b"POST";
const HTTP_1_0: &[u8] = b"HTTP/1.0";
const HTTP_1_1: &[u8] = b"HTTP/1.1";
const RELAY_API_PATH_PREFIX: &[u8] = b"/api/relays/";
const RELAY_ON_PATH_SUFFIX: &[u8] = b"/on";
const RELAY_OFF_PATH_SUFFIX: &[u8] = b"/off";

const INDEX_HTML: &[u8] = br#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Relay Controller</title></head>
<body><h1>Relay Controller</h1><p>Use the HTTP API to control relays.</p></body></html>"#;

/// A TCP/HTTP server worker. Spawn several instances for concurrent clients;
/// each worker gets its own `TcpSocket`, while all share the same `Stack`.
#[embassy_executor::task(pool_size = 4)]
pub async fn http_server_task(stack: Stack<'static>, relays: RelayControl) -> ! {
    let mut rx_buffer = [0_u8; SOCKET_BUFFER_SIZE];
    let mut tx_buffer = [0_u8; SOCKET_BUFFER_SIZE];

    // This creates one logical TCP socket from the `StackResources` pool.
    // The task owns this socket and its two buffers for its entire lifetime.
    // `pool_size = 4` permits four such workers to be spawned.
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

    socket.set_timeout(Some(Duration::from_secs(10)));

    loop {
        if let Err(error) = socket.accept(HTTP_PORT).await {
            warn!("HTTP accept failed: {:?}", error);
            recycle_socket(&mut socket).await;
            Timer::after_millis(50).await;
            continue;
        }
        let peer = socket.remote_endpoint();
        info!("HTTP connection: {:?}", peer);

        match with_timeout(Duration::from_secs(10), serve_request(&mut socket, relays)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!("HTTP {:?}: I/O error: {:?}", peer, error),
            Err(_) => warn!("HTTP {:?}: request/response timed out", peer),
        }
        recycle_socket(&mut socket).await;
    }
}

/// Let FIN complete before listening again. An immediate accept after close
/// returns InvalidState and can spin forever, starving the network runner.
async fn recycle_socket(socket: &mut TcpSocket<'_>) {
    socket.close();
    let closed = with_timeout(Duration::from_secs(2), async {
        socket.flush().await?;
        while !matches!(
            socket.state(),
            embassy_net::tcp::State::Closed | embassy_net::tcp::State::TimeWait
        ) {
            Timer::after_millis(10).await;
        }
        Ok::<(), embassy_net::tcp::Error>(())
    })
    .await;
    if !matches!(closed, Ok(Ok(()))) {
        warn!("HTTP close incomplete; aborting socket");
    }
    socket.abort();
    let _ = with_timeout(Duration::from_secs(1), socket.flush()).await;
}

async fn serve_request(
    socket: &mut TcpSocket<'_>,
    relays: RelayControl,
) -> Result<(), embassy_net::tcp::Error> {
    let mut request_buffer = [0_u8; REQUEST_BUFFER_SIZE];
    let header_byte_count = read_http_headers(socket, &mut request_buffer).await;
    let response = match header_byte_count {
        Some(byte_count) => {
            let request = &request_buffer[..byte_count];
            if let Some(line) = parse_http_request_line(request) {
                info!(
                    "HTTP {:?} request: {:?} {:?}",
                    socket.remote_endpoint(),
                    core::str::from_utf8(line.method).unwrap_or("<invalid UTF-8>"),
                    core::str::from_utf8(line.target).unwrap_or("<invalid UTF-8>")
                );
            } else {
                warn!("HTTP malformed request line");
            }
            route(request, relays).await
        }
        None => {
            warn!("HTTP incomplete, oversized, or unreadable headers");
            Response::BadRequest
        }
    };

    match response {
        Response::Html => {
            respond(socket, b"200 OK", b"text/html; charset=utf-8", INDEX_HTML).await?
        }
        Response::RelayState => {
            let mut body = [0_u8; 96];
            let body_len = write_relay_state(&mut body, relays.state_mask());
            respond(socket, b"200 OK", b"application/json", &body[..body_len]).await?;
        }
        Response::CommandAccepted => {
            respond(
                socket,
                b"202 Accepted",
                b"application/json",
                b"{\"accepted\":true}",
            )
            .await?
        }
        Response::BadRequest => {
            respond(
                socket,
                b"400 Bad Request",
                b"application/json",
                b"{\"error\":\"invalid relay request\"}",
            )
            .await?
        }
        Response::NotFound => {
            respond(
                socket,
                b"404 Not Found",
                b"application/json",
                b"{\"error\":\"not found\"}",
            )
            .await?
        }
    }
    socket.flush().await?;
    info!("HTTP response acknowledged: {:?}", socket.remote_endpoint());
    Ok(())
}

enum Response {
    Html,
    RelayState,
    CommandAccepted,
    BadRequest,
    NotFound,
}

/// Read until the complete HTTP header block has arrived.
///
/// HTTP headers end with `\r\n\r\n`: one CRLF terminates each header line,
/// and the empty line after the final header contributes the second CRLF.
async fn read_http_headers(socket: &mut TcpSocket<'_>, buffer: &mut [u8]) -> Option<usize> {
    let mut received_byte_count = 0;

    while received_byte_count < buffer.len() {
        let bytes_read = socket.read(&mut buffer[received_byte_count..]).await.ok()?;
        if bytes_read == 0 {
            return None;
        }
        received_byte_count += bytes_read;

        if contains_bytes(&buffer[..received_byte_count], END_OF_HTTP_HEADERS) {
            return Some(received_byte_count);
        }
    }
    None
}

async fn route(request: &[u8], relays: RelayControl) -> Response {
    let Some(request_line) = parse_http_request_line(request) else {
        return Response::BadRequest;
    };

    if !is_supported_http_version(request_line.version) {
        return Response::BadRequest;
    }

    if request_line.method == HTTP_GET && request_line.target == b"/" {
        return Response::Html;
    }
    if request_line.method == HTTP_GET
        && matches!(request_line.target, b"/api/relays" | b"/api/relays/")
    {
        return Response::RelayState;
    }

    if request_line.method != HTTP_POST {
        return Response::NotFound;
    }
    if request_line.target == b"/api/relays/all/on" {
        relays.all_on().await;
        return Response::CommandAccepted;
    }
    if request_line.target == b"/api/relays/all/off" {
        relays.all_off().await;
        return Response::CommandAccepted;
    }
    let Some(path_after_relay_prefix) = request_line.target.strip_prefix(RELAY_API_PATH_PREFIX)
    else {
        return Response::NotFound;
    };
    let Some((index, action_path)) = parse_relay_target(path_after_relay_prefix) else {
        return Response::BadRequest;
    };
    let on = match action_path {
        RELAY_ON_PATH_SUFFIX => true,
        RELAY_OFF_PATH_SUFFIX => false,
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

/// Parse `<relay index>/<action>` after `/api/relays/`.
///
/// For `/api/relays/12/on`, this returns `(12, b"/on")`.
fn parse_relay_target(path_after_relay_prefix: &[u8]) -> Option<(usize, &[u8])> {
    let relay_index_byte_count = path_after_relay_prefix
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();

    if relay_index_byte_count == 0 {
        return None;
    }

    let relay_index_bytes = &path_after_relay_prefix[..relay_index_byte_count];
    let action_path = &path_after_relay_prefix[relay_index_byte_count..];

    let mut relay_index = 0_usize;
    for digit in relay_index_bytes {
        relay_index = relay_index.checked_mul(10)?;
        relay_index = relay_index.checked_add((digit - b'0') as usize)?;
    }
    Some((relay_index, action_path))
}

struct HttpRequestLine<'a> {
    method: &'a [u8],
    target: &'a [u8],
    version: &'a [u8],
}

/// Extract `METHOD`, `TARGET`, and `HTTP VERSION` from the first HTTP line.
fn parse_http_request_line(request: &[u8]) -> Option<HttpRequestLine<'_>> {
    let request_line_end = request
        .windows(CRLF.len())
        .position(|bytes| bytes == CRLF)?;
    let request_line = &request[..request_line_end];

    let method_end = request_line
        .iter()
        .position(|byte| *byte == HTTP_FIELD_SEPARATOR)?;
    let method = &request_line[..method_end];
    let after_method = &request_line[method_end + 1..];

    let target_end = after_method
        .iter()
        .position(|byte| *byte == HTTP_FIELD_SEPARATOR)?;
    let target = &after_method[..target_end];
    let version = &after_method[target_end + 1..];

    Some(HttpRequestLine {
        method,
        target,
        version,
    })
}

fn is_supported_http_version(version: &[u8]) -> bool {
    version == HTTP_1_0 || version == HTTP_1_1
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

async fn respond(
    socket: &mut TcpSocket<'_>,
    status: &[u8],
    content_type: &[u8],
    body: &[u8],
) -> Result<(), embassy_net::tcp::Error> {
    info!(
        "HTTP {:?} response: {}",
        socket.remote_endpoint(),
        core::str::from_utf8(status).unwrap_or("<invalid status>")
    );
    socket.write_all(b"HTTP/1.1 ").await?;
    socket.write_all(status).await?;
    socket.write_all(b"\r\nContent-Type: ").await?;
    socket.write_all(content_type).await?;
    socket.write_all(b"\r\nConnection: close\r\n\r\n").await?;
    socket.write_all(body).await?;
    Ok(())
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
