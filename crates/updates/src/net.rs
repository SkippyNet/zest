//! The one place Zest talks to the internet from.
//!
//! Two features use it: the check for a newer Zest release and the download
//! of the public package catalog for PCs without the Windows Package Manager.
//! Every request is an anonymous HTTPS `GET` that carries a user agent with
//! the Zest version and nothing else: no account, no cookies, no identifier
//! and nothing about the PC or its user.
//!
//! Certificates are verified by Windows itself (Schannel with the system
//! certificate store), so organisations that deploy their own roots are
//! respected and no certificate bundle is shipped inside the executable.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;
use zest_core::{Progress, brand};

/// Why a request did not produce a usable answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    /// The server could not be reached: no connection, DNS failure, timeout.
    #[error("the server could not be reached ({0})")]
    Unreachable(String),
    /// The server answered with an HTTP error status.
    #[error("the server answered with status {status}")]
    Status {
        /// The HTTP status code.
        status: u16,
        /// Seconds to wait before asking again, when the server said so.
        retry_after_secs: Option<u64>,
    },
    /// The answer was larger than this kind of answer is ever expected to be.
    #[error("the download is larger than expected ({limit} bytes allowed)")]
    TooLarge {
        /// The size limit that was exceeded.
        limit: u64,
    },
    /// The connection broke while the answer was being received.
    #[error("the download was interrupted ({0})")]
    Interrupted(String),
    /// A local file could not be written.
    #[error("could not write {path}: {reason}")]
    File {
        /// The file being written.
        path: String,
        /// What went wrong.
        reason: String,
    },
    /// Only encrypted connections are allowed.
    #[error("refusing to use an unencrypted address: {0}")]
    Insecure(String),
    /// The operation was cancelled.
    #[error("cancelled")]
    Cancelled,
}

impl NetError {
    /// True for the "resource does not exist" status.
    pub fn is_not_found(&self) -> bool {
        matches!(self, NetError::Status { status: 404, .. })
    }

    /// True when the server asks clients to slow down.
    pub fn is_rate_limited(&self) -> bool {
        matches!(
            self,
            NetError::Status {
                status: 403 | 429,
                ..
            }
        )
    }
}

/// Outcome of a download to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Downloaded {
    /// Number of bytes written.
    pub bytes: u64,
    /// Lower-case hexadecimal SHA-256 of what was written.
    pub sha256: String,
}

/// A small HTTPS client with fixed timeouts and an anonymous user agent.
pub(crate) struct Client {
    quick: ureq::Agent,
    bulk: ureq::Agent,
    /// Permit `http://` addresses. Only tests, which talk to a server on the
    /// loopback interface, switch this on.
    allow_insecure: bool,
}

/// The user agent sent with every request, e.g. `Zest/0.9.0`.
pub(crate) fn user_agent() -> String {
    format!("{}/{}", brand::NAME, brand::VERSION)
}

fn agent(body_timeout: Duration) -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    let tls = TlsConfig::builder()
        .provider(TlsProvider::NativeTls)
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .user_agent(user_agent())
        .http_status_as_error(false)
        .max_redirects(8)
        .timeout_resolve(Some(Duration::from_secs(15)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(body_timeout))
        .build()
        .into()
}

fn describe(error: &ureq::Error) -> String {
    match error {
        ureq::Error::HostNotFound => "the address could not be resolved".to_owned(),
        ureq::Error::Timeout(_) => "the server took too long to answer".to_owned(),
        ureq::Error::ConnectionFailed => "the connection failed".to_owned(),
        other => other.to_string(),
    }
}

impl Client {
    /// A client for real use: HTTPS only.
    pub(crate) fn new() -> Client {
        Client {
            quick: agent(Duration::from_secs(60)),
            bulk: agent(Duration::from_secs(30 * 60)),
            allow_insecure: false,
        }
    }

    /// A client that also accepts `http://` addresses (loopback test servers).
    #[cfg(test)]
    pub(crate) fn for_tests() -> Client {
        Client {
            allow_insecure: true,
            ..Client::new()
        }
    }

    fn check_scheme(&self, url: &str) -> Result<(), NetError> {
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("https://") || (self.allow_insecure && lower.starts_with("http://")) {
            Ok(())
        } else {
            Err(NetError::Insecure(url.to_owned()))
        }
    }

    fn send(
        &self,
        agent: &ureq::Agent,
        url: &str,
        accept: Option<&str>,
    ) -> Result<ureq::http::Response<ureq::Body>, NetError> {
        self.check_scheme(url)?;
        let mut request = agent.get(url);
        if let Some(accept) = accept {
            request = request.header("Accept", accept);
        }
        let response = request
            .call()
            .map_err(|e| NetError::Unreachable(describe(&e)))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let header = |name: &str| {
                response
                    .headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            };
            // GitHub announces the end of a rate limit window as a point in
            // time; other servers send a plain number of seconds.
            let retry_after_secs = header("retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .or_else(|| {
                    let reset = header("x-ratelimit-reset")?.trim().parse::<u64>().ok()?;
                    let exhausted = header("x-ratelimit-remaining")?.trim() == "0";
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()?
                        .as_secs();
                    exhausted.then(|| reset.saturating_sub(now))
                });
            return Err(NetError::Status {
                status,
                retry_after_secs,
            });
        }
        Ok(response)
    }

    /// Fetches a small text document (JSON, checksum lists).
    pub(crate) fn get_text(
        &self,
        url: &str,
        accept: Option<&str>,
        limit: u64,
    ) -> Result<String, NetError> {
        let mut response = self.send(&self.quick, url, accept)?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| NetError::Interrupted(e.to_string()))?;
        if bytes.len() as u64 > limit {
            return Err(NetError::TooLarge { limit });
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Downloads `url` into the file `dest`, replacing it.
    ///
    /// `progress` receives the byte counts and is polled for cancellation. A
    /// download that fails or is cancelled leaves no partial file behind.
    pub(crate) fn download(
        &self,
        url: &str,
        dest: &Path,
        limit: u64,
        progress: &Progress,
    ) -> Result<Downloaded, NetError> {
        let result = self.download_inner(url, dest, limit, progress);
        if result.is_err() {
            // Our own partial download.
            let _ = std::fs::remove_file(dest);
        }
        result
    }

    fn download_inner(
        &self,
        url: &str,
        dest: &Path,
        limit: u64,
        progress: &Progress,
    ) -> Result<Downloaded, NetError> {
        if progress.is_cancelled() {
            return Err(NetError::Cancelled);
        }
        let mut response = self.send(&self.bulk, url, Some("application/octet-stream"))?;
        let expected = response.body().content_length();
        if expected.is_some_and(|len| len > limit) {
            return Err(NetError::TooLarge { limit });
        }
        if let Some(len) = expected {
            progress.set_total(len);
        }
        progress.set_done(0);

        let file_error = |e: std::io::Error| NetError::File {
            path: dest.display().to_string(),
            reason: e.to_string(),
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(file_error)?;
        }
        let mut file = std::fs::File::create(dest).map_err(file_error)?;
        let mut reader = response.body_mut().as_reader();
        let mut hasher = Sha256::new();
        let mut chunk = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            if progress.is_cancelled() {
                return Err(NetError::Cancelled);
            }
            let read = reader
                .read(&mut chunk)
                .map_err(|e| NetError::Interrupted(e.to_string()))?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > limit {
                return Err(NetError::TooLarge { limit });
            }
            hasher.update(&chunk[..read]);
            file.write_all(&chunk[..read]).map_err(file_error)?;
            progress.set_done(total);
            progress.add_bytes(read as u64);
        }
        file.flush().map_err(file_error)?;
        if expected.is_some_and(|len| len != total) {
            return Err(NetError::Interrupted(format!(
                "received {total} of {} bytes",
                expected.unwrap_or_default()
            )));
        }
        Ok(Downloaded {
            bytes: total,
            sha256: hex(&hasher.finalize()),
        })
    }
}

/// Lower-case hexadecimal text of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// SHA-256 of a file, as lower-case hexadecimal text.
#[cfg(test)]
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// A tiny HTTP/1.1 server on the loopback interface for tests: it serves a
/// fixed set of paths and records what clients sent.
#[cfg(test)]
pub(crate) mod testserver {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// One canned answer.
    #[derive(Clone, Debug)]
    pub(crate) struct Reply {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
        /// Announce this many more bytes than are actually sent, then close
        /// the connection: a download that breaks off half-way.
        pub truncate_by: usize,
    }

    impl Reply {
        pub(crate) fn ok(body: impl Into<Vec<u8>>) -> Reply {
            Reply {
                status: 200,
                headers: Vec::new(),
                body: body.into(),
                truncate_by: 0,
            }
        }

        pub(crate) fn status(status: u16) -> Reply {
            Reply {
                status,
                headers: Vec::new(),
                body: b"{}".to_vec(),
                truncate_by: 0,
            }
        }

        pub(crate) fn header(mut self, name: &str, value: &str) -> Reply {
            self.headers.push((name.to_owned(), value.to_owned()));
            self
        }
    }

    /// A request as the server received it.
    #[derive(Clone, Debug, Default)]
    pub(crate) struct Seen {
        pub path: String,
        /// Header names in lower case.
        pub headers: HashMap<String, String>,
    }

    pub(crate) struct TestServer {
        pub base: String,
        routes: Arc<Mutex<HashMap<String, Reply>>>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl TestServer {
        pub(crate) fn start() -> TestServer {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let base = format!("http://{}", listener.local_addr().expect("local address"));
            let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
            let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
            let (thread_routes, thread_seen) = (routes.clone(), seen.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let routes = thread_routes.clone();
                    let seen = thread_seen.clone();
                    std::thread::spawn(move || serve(stream, &routes, &seen));
                }
            });
            TestServer { base, routes, seen }
        }

        pub(crate) fn route(&self, path: &str, reply: Reply) {
            self.routes.lock().unwrap().insert(path.to_owned(), reply);
        }

        pub(crate) fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }

        pub(crate) fn requests(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn serve(stream: TcpStream, routes: &Mutex<HashMap<String, Reply>>, seen: &Mutex<Vec<Seen>>) {
        let Ok(clone) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(clone);
        let mut stream = stream;
        loop {
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                return;
            }
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .to_owned();
            let mut headers = HashMap::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    headers.insert(name.trim().to_lowercase(), value.trim().to_owned());
                }
            }
            seen.lock().unwrap().push(Seen {
                path: path.clone(),
                headers,
            });
            let reply = routes
                .lock()
                .unwrap()
                .get(&path)
                .cloned()
                .unwrap_or_else(|| Reply::status(404));
            let reason = match reply.status {
                200 => "OK",
                302 => "Found",
                403 => "Forbidden",
                404 => "Not Found",
                429 => "Too Many Requests",
                _ => "Status",
            };
            let mut head = format!(
                "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\n",
                reply.status,
                reply.body.len() + reply.truncate_by
            );
            for (name, value) in &reply.headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            if stream.write_all(head.as_bytes()).is_err() || stream.write_all(&reply.body).is_err()
            {
                return;
            }
            let _ = stream.flush();
            if reply.truncate_by > 0 {
                // Closing here leaves the client short of the announced length.
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testserver::{Reply, TestServer};
    use super::*;

    #[test]
    fn text_is_fetched_with_an_anonymous_user_agent() {
        let server = TestServer::start();
        server.route("/hello", Reply::ok("hi there"));
        let client = Client::for_tests();
        let text = client
            .get_text(&server.url("/hello"), Some("text/plain"), 1024)
            .unwrap();
        assert_eq!(text, "hi there");

        let seen = server.requests();
        assert_eq!(seen.len(), 1);
        let headers = &seen[0].headers;
        assert_eq!(headers.get("user-agent"), Some(&user_agent()));
        assert_eq!(
            headers.get("accept").map(String::as_str),
            Some("text/plain")
        );
        // Nothing that could identify the user or the PC travels along.
        for name in [
            "cookie",
            "authorization",
            "referer",
            "from",
            "x-forwarded-for",
        ] {
            assert!(!headers.contains_key(name), "unexpected header {name}");
        }
        let user = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
        let machine = std::env::var("COMPUTERNAME")
            .unwrap_or_default()
            .to_lowercase();
        for (name, value) in headers {
            // The host header names the loopback test server, nothing else.
            if name == "host" {
                continue;
            }
            let value = value.to_lowercase();
            assert!(user.len() < 3 || !value.contains(&user), "{name}: {value}");
            assert!(
                machine.len() < 3 || !value.contains(&machine),
                "{name}: {value}"
            );
        }
    }

    #[test]
    fn error_statuses_are_reported_with_retry_hints() {
        let server = TestServer::start();
        server.route("/gone", Reply::status(404));
        server.route("/slow", Reply::status(429).header("Retry-After", "120"));
        let reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600;
        server.route(
            "/limited",
            Reply::status(403)
                .header("X-RateLimit-Remaining", "0")
                .header("X-RateLimit-Reset", &reset.to_string()),
        );
        server.route("/forbidden", Reply::status(403));
        let client = Client::for_tests();

        let gone = client
            .get_text(&server.url("/gone"), None, 1024)
            .unwrap_err();
        assert!(gone.is_not_found() && !gone.is_rate_limited());

        let slow = client
            .get_text(&server.url("/slow"), None, 1024)
            .unwrap_err();
        assert_eq!(
            slow,
            NetError::Status {
                status: 429,
                retry_after_secs: Some(120)
            }
        );
        assert!(slow.is_rate_limited());

        match client
            .get_text(&server.url("/limited"), None, 1024)
            .unwrap_err()
        {
            NetError::Status {
                status: 403,
                retry_after_secs: Some(wait),
            } => assert!((500..=600).contains(&wait), "{wait}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            client
                .get_text(&server.url("/forbidden"), None, 1024)
                .unwrap_err(),
            NetError::Status {
                status: 403,
                retry_after_secs: None
            }
        );
    }

    #[test]
    fn oversized_and_unencrypted_answers_are_refused() {
        let server = TestServer::start();
        server.route("/big", Reply::ok(vec![b'x'; 5000]));
        let client = Client::for_tests();
        assert_eq!(
            client.get_text(&server.url("/big"), None, 100).unwrap_err(),
            NetError::TooLarge { limit: 100 }
        );
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("big.bin");
        let error = client
            .download(&server.url("/big"), &dest, 100, &Progress::new())
            .unwrap_err();
        assert_eq!(error, NetError::TooLarge { limit: 100 });
        assert!(!dest.exists());

        // The real client never speaks plain HTTP.
        let strict = Client::new();
        assert!(matches!(
            strict.get_text(&server.url("/big"), None, 10_000),
            Err(NetError::Insecure(_))
        ));
        assert!(matches!(
            strict.get_text("ftp://example.invalid/x", None, 10),
            Err(NetError::Insecure(_))
        ));
    }

    #[test]
    fn downloads_are_hashed_and_counted() {
        let server = TestServer::start();
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        server.route("/file", Reply::ok(body.clone()));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("nested").join("file.bin");
        let progress = Progress::new();
        let done = Client::for_tests()
            .download(&server.url("/file"), &dest, 1 << 20, &progress)
            .unwrap();
        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(done.sha256, hex(&Sha256::digest(&body)));
        assert_eq!(done.sha256, sha256_file(&dest).unwrap());
        assert_eq!(progress.done(), body.len() as u64);
        assert_eq!(progress.total(), body.len() as u64);
    }

    #[test]
    fn broken_and_cancelled_downloads_leave_no_file() {
        let server = TestServer::start();
        let mut cut = Reply::ok(vec![7u8; 4096]);
        cut.truncate_by = 4096;
        server.route("/cut", cut);
        server.route("/file", Reply::ok(vec![1u8; 4096]));
        let dir = tempfile::tempdir().unwrap();
        let client = Client::for_tests();

        let dest = dir.path().join("cut.bin");
        let error = client
            .download(&server.url("/cut"), &dest, 1 << 20, &Progress::new())
            .unwrap_err();
        assert!(matches!(error, NetError::Interrupted(_)), "{error:?}");
        assert!(!dest.exists());

        let cancelled = Progress::new();
        cancelled.cancel();
        let dest = dir.path().join("cancelled.bin");
        assert_eq!(
            client
                .download(&server.url("/file"), &dest, 1 << 20, &cancelled)
                .unwrap_err(),
            NetError::Cancelled
        );
        assert!(!dest.exists());

        let missing = client
            .download(&server.url("/nope"), &dest, 1 << 20, &Progress::new())
            .unwrap_err();
        assert!(missing.is_not_found());
    }

    #[test]
    fn unreachable_servers_are_reported_as_such() {
        // A loopback port nobody listens on.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = free.local_addr().unwrap();
        drop(free);
        let error = Client::for_tests()
            .get_text(&format!("http://{address}/x"), None, 100)
            .unwrap_err();
        assert!(matches!(error, NetError::Unreachable(_)), "{error:?}");
    }
}
