//! Bounded, cancellable loopback rebuild transport for the native project host.
//! Compiler refusals consume edits; transport failures and host overload retry.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_RESPONSE_BYTES: usize = 128 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Rebuild {
    Applied,
    Refused { status: u16, detail: String },
}

pub(super) struct Client {
    address: SocketAddr,
    capability: String,
    timeout: Duration,
}

impl Client {
    pub(super) fn new(
        address: SocketAddr,
        capability: String,
        timeout: Duration,
    ) -> io::Result<Self> {
        if !address.ip().is_loopback()
            || address.port() == 0
            || capability.len() != 64
            || !capability.bytes().all(|byte| byte.is_ascii_hexdigit())
            || timeout.is_zero()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid loopback rebuild address, capability or deadline",
            ));
        }
        Ok(Self {
            address,
            capability,
            timeout,
        })
    }

    pub(super) fn restart(&self, stopped: &dyn Fn() -> bool) -> io::Result<Rebuild> {
        let deadline = Instant::now().checked_add(self.timeout).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "rebuild deadline overflow")
        })?;
        check(deadline, stopped)?;
        let mut socket = TcpStream::connect_timeout(
            &self.address,
            POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
        )?;
        socket.set_read_timeout(Some(POLL_INTERVAL))?;
        socket.set_write_timeout(Some(POLL_INTERVAL))?;
        let request = format!(
            "POST /api/restart?cap={} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            self.capability, self.address, self.address,
        );
        let mut remaining = request.as_bytes();
        while !remaining.is_empty() {
            check(deadline, stopped)?;
            match socket.write(remaining) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => remaining = &remaining[count..],
                Err(error) if retryable(&error) => {}
                Err(error) => return Err(error),
            }
        }
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let mut framing = None;
        loop {
            check(deadline, stopped)?;
            match socket.read(&mut buffer) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated rebuild response",
                    ));
                }
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if retryable(&error) => continue,
                Err(error) => return Err(error),
            }
            if bytes.len() > MAX_RESPONSE_BYTES {
                return Err(invalid("rebuild response exceeded its byte limit"));
            }
            if framing.is_none() {
                framing = response_framing(&bytes)?;
            }
            if let Some((status, body, length)) = framing {
                let end = body + length;
                if bytes.len() > end {
                    return Err(invalid("rebuild response has trailing bytes"));
                }
                if bytes.len() == end {
                    return if status == 200 {
                        Ok(Rebuild::Applied)
                    } else {
                        let detail = String::from_utf8_lossy(&bytes[body..])
                            .replace(&self.capability, "[redacted]")
                            .chars()
                            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                            .take(4096)
                            .collect();
                        Ok(Rebuild::Refused { status, detail })
                    };
                }
            }
        }
    }
}

fn check(deadline: Instant, stopped: &dyn Fn() -> bool) -> io::Result<()> {
    if stopped() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "source watcher stopped",
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "rebuild response deadline exceeded",
        ));
    }
    Ok(())
}

fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn response_framing(bytes: &[u8]) -> io::Result<Option<(u16, usize, usize)>> {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        if bytes.len() > MAX_HEADER_BYTES {
            return Err(invalid(
                "rebuild response headers exceeded their byte limit",
            ));
        }
        return Ok(None);
    };
    let body = header_end + 4;
    if body > MAX_HEADER_BYTES {
        return Err(invalid(
            "rebuild response headers exceeded their byte limit",
        ));
    }
    let header = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| invalid("invalid rebuild response headers"))?;
    let mut lines = header.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| invalid("missing rebuild status"))?;
    let mut status_parts = status_line.splitn(3, ' ');
    if status_parts.next() != Some("HTTP/1.1") {
        return Err(invalid("invalid rebuild HTTP version"));
    }
    let status_text = status_parts
        .next()
        .ok_or_else(|| invalid("missing rebuild status"))?;
    if status_text.len() != 3 || !status_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("invalid rebuild status code"));
    }
    let status = status_text
        .parse::<u16>()
        .map_err(|_| invalid("invalid rebuild status code"))?;
    if !(200..=599).contains(&status) {
        return Err(invalid("unsupported rebuild status code"));
    }
    let mut length = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("invalid rebuild header"))?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(invalid("unsupported rebuild transfer encoding"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            if length.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(invalid("invalid or duplicate rebuild content length"));
            }
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| invalid("invalid rebuild content length"))?,
            );
        }
    }
    let length = length.ok_or_else(|| invalid("missing rebuild content length"))?;
    if length > MAX_RESPONSE_BYTES - body {
        return Err(invalid("rebuild response exceeded its byte limit"));
    }
    Ok(Some((status, body, length)))
}

#[derive(Default)]
pub(super) struct PendingReload {
    pending: bool,
    retry_at: Duration,
}

impl PendingReload {
    pub(super) fn observe(&mut self, changed: bool) {
        if changed {
            self.pending = true;
            self.retry_at = Duration::ZERO;
        }
    }

    pub(super) fn ready(&self, now: Duration) -> bool {
        self.pending && now >= self.retry_at
    }

    pub(super) fn complete(&mut self, now: Duration, result: &io::Result<Rebuild>) {
        self.pending = match result {
            Err(_) => true,
            Ok(Rebuild::Refused { status, .. }) => matches!(status, 429 | 503),
            Ok(Rebuild::Applied) => false,
        };
        self.retry_at = now.saturating_add(RETRY_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    const CAP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn listener() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").unwrap()
    }

    fn request(socket: &mut TcpStream) -> Vec<u8> {
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut byte = [0];
        while !bytes.ends_with(b"\r\n\r\n") {
            assert_eq!(socket.read(&mut byte).unwrap(), 1);
            bytes.push(byte[0]);
            assert!(bytes.len() < 4096);
        }
        bytes
    }

    fn exchange(response: Vec<u8>) -> io::Result<Rebuild> {
        let listener = listener();
        let client = Client::new(
            listener.local_addr().unwrap(),
            CAP.into(),
            Duration::from_secs(3),
        )
        .unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            request(&mut socket);
            let _ = socket.write_all(&response);
        });
        let result = client.restart(&|| false);
        server.join().unwrap();
        result
    }

    #[test]
    fn real_request_has_crlf_origin_and_capability() {
        let listener = listener();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let bytes = request(&mut socket);
            assert_eq!(
                String::from_utf8(bytes).unwrap(),
                format!(
                    "POST /api/restart?cap={CAP} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            );
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
        });
        let client = Client::new(address, CAP.into(), Duration::from_secs(3)).unwrap();
        assert_eq!(client.restart(&|| false).unwrap(), Rebuild::Applied);
        server.join().unwrap();
    }

    #[test]
    fn complete_response_does_not_wait_for_connection_close() {
        let listener = listener();
        let client = Client::new(
            listener.local_addr().unwrap(),
            CAP.into(),
            Duration::from_secs(3),
        )
        .unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            request(&mut socket);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            received.recv_timeout(Duration::from_secs(4)).unwrap();
        });
        assert_eq!(client.restart(&|| false).unwrap(), Rebuild::Applied);
        sent.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn compiler_failure_preserves_status_and_redacts_capability() {
        let body = format!("compile failed: {CAP}");
        let reply = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(
            exchange(reply.into_bytes()).unwrap(),
            Rebuild::Refused {
                status: 500,
                detail: "compile failed: [redacted]".into(),
            }
        );
    }

    #[test]
    fn partial_success_response_is_not_success() {
        assert_eq!(
            exchange(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort".to_vec())
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn response_header_and_body_limits_are_enforced_before_unbounded_reading() {
        assert!(response_framing(&vec![b'x'; MAX_HEADER_BYTES + 1]).is_err());
        assert!(
            response_framing(b"HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\n")
                .is_err()
        );
        let bytes = format!("HTTP/1.1 200 OK\r\nContent-Length: {MAX_RESPONSE_BYTES}\r\n\r\n");
        assert!(response_framing(bytes.as_bytes()).is_err());
    }

    #[test]
    fn malformed_framing_cannot_publish_success() {
        for bytes in [
            b"HTTP/1.1 2000 OK\r\nContent-Length: 0\r\n\r\n".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: +1\r\n\r\n",
            b"HTTP/1.1 200 OK\r\n\r\n",
            b"HTTP/2 200 OK\r\nContent-Length: 0\r\n\r\n",
        ] {
            assert!(response_framing(bytes).is_err(), "{bytes:?}");
        }
    }

    #[test]
    fn silent_server_obeys_absolute_deadline() {
        let listener = listener();
        let client = Client::new(
            listener.local_addr().unwrap(),
            CAP.into(),
            Duration::from_millis(200),
        )
        .unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            request(&mut socket);
            received.recv_timeout(Duration::from_secs(4)).unwrap();
        });
        let started = Instant::now();
        assert_eq!(
            client.restart(&|| false).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        sent.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn shutdown_interrupts_an_in_flight_rebuild_read() {
        let listener = listener();
        let client = Client::new(
            listener.local_addr().unwrap(),
            CAP.into(),
            Duration::from_secs(300),
        )
        .unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stopped);
        let (sent, received) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            request(&mut socket);
            server_stop.store(true, Ordering::Release);
            received.recv_timeout(Duration::from_secs(4)).unwrap();
        });
        let started = Instant::now();
        assert_eq!(
            client
                .restart(&|| stopped.load(Ordering::Acquire))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        sent.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn stopped_request_does_not_connect() {
        let listener = listener();
        let client = Client::new(
            listener.local_addr().unwrap(),
            CAP.into(),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            client.restart(&|| true).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn transport_failure_retries_without_a_second_edit() {
        let mut pending = PendingReload::default();
        assert!(!pending.ready(Duration::ZERO));
        pending.observe(true);
        assert!(pending.ready(Duration::ZERO));
        pending.complete(Duration::ZERO, &Err(io::ErrorKind::ConnectionReset.into()));
        pending.observe(false);
        assert!(!pending.ready(Duration::from_millis(999)));
        assert!(pending.ready(Duration::from_secs(1)));
        pending.complete(Duration::from_secs(1), &Ok(Rebuild::Applied));
        assert!(!pending.ready(Duration::from_secs(30)));
    }

    #[test]
    fn compiler_failure_waits_for_an_edit_instead_of_rebuild_looping() {
        let mut pending = PendingReload::default();
        pending.observe(true);
        pending.complete(
            Duration::ZERO,
            &Ok(Rebuild::Refused {
                status: 500,
                detail: "syntax error".into(),
            }),
        );
        assert!(!pending.ready(Duration::from_secs(30)));
        pending.observe(true);
        assert!(pending.ready(Duration::from_secs(30)));
    }

    #[test]
    fn new_edit_bypasses_transport_retry_delay() {
        let mut pending = PendingReload::default();
        pending.observe(true);
        pending.complete(
            Duration::ZERO,
            &Err(io::ErrorKind::ConnectionRefused.into()),
        );
        pending.observe(true);
        assert!(pending.ready(Duration::from_millis(1)));
    }

    #[test]
    fn address_and_capability_are_validated_without_network_access() {
        let address = "127.0.0.1:1234".parse().unwrap();
        for cap in [
            "".to_owned(),
            "f".repeat(63),
            format!("{}\r", "f".repeat(63)),
        ] {
            assert!(Client::new(address, cap, Duration::from_secs(1)).is_err());
        }
        assert!(
            Client::new(
                "192.0.2.1:1234".parse().unwrap(),
                CAP.into(),
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(Client::new(address, CAP.into(), Duration::ZERO).is_err());
        assert!(
            Client::new(
                "[::1]:1234".parse().unwrap(),
                CAP.into(),
                Duration::from_secs(1)
            )
            .is_ok()
        );
    }

    #[test]
    fn host_backpressure_keeps_the_edit_pending_until_success() {
        for status in [429, 503] {
            let mut pending = PendingReload::default();
            pending.observe(true);
            pending.complete(
                Duration::ZERO,
                &Ok(Rebuild::Refused {
                    status,
                    detail: "busy".into(),
                }),
            );
            assert!(!pending.ready(Duration::from_millis(999)));
            assert!(pending.ready(Duration::from_secs(1)));
            pending.complete(Duration::from_secs(1), &Ok(Rebuild::Applied));
            assert!(!pending.ready(Duration::from_secs(2)));
        }
    }

    #[test]
    fn real_overload_responses_do_not_drop_the_pending_source_change() {
        for status in [429, 503] {
            let response =
                format!("HTTP/1.1 {status} Busy\r\nContent-Length: 4\r\n\r\nbusy");
            let refused = exchange(response.into_bytes());
            assert_eq!(
                refused.as_ref().unwrap(),
                &Rebuild::Refused {
                    status,
                    detail: "busy".into(),
                }
            );
            let mut pending = PendingReload::default();
            pending.observe(true);
            pending.complete(Duration::ZERO, &refused);
            pending.observe(false);
            assert!(pending.ready(Duration::from_secs(1)));
            let applied = exchange(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec());
            assert_eq!(applied.as_ref().unwrap(), &Rebuild::Applied);
            pending.complete(Duration::from_secs(1), &applied);
            assert!(!pending.ready(Duration::from_secs(2)));
        }
    }
}
