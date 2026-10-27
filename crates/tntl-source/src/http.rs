use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use thiserror::Error;
use tntl_store::{StoreError, Verify, verify_file};

use crate::endpoints;

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("http status {status} for {url}")]
    Status { status: u16, url: String },
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("hash mismatch for {path}: expected {expected}, got {actual}")]
    Hash {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("unknown version: {0}")]
    UnknownVersion(String),
    #[error("download failed after {attempts} attempts: {url}")]
    Retries { attempts: u32, url: String },
    #[error("download body was cut off: {0}")]
    Body(String),
    #[error("invalid proxy {url}: {message}")]
    Proxy { url: String, message: String },
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Loader(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn io_error(path: &Path, source: std::io::Error) -> SourceError {
    SourceError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub dest: PathBuf,
    pub sha1: Option<String>,
    pub sha512: Option<String>,
    pub size: Option<u64>,
}

impl DownloadRequest {
    pub fn new(url: impl Into<String>, dest: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            dest: dest.into(),
            sha1: None,
            sha512: None,
            size: None,
        }
    }

    pub fn with_sha1(mut self, sha1: impl Into<String>) -> Self {
        self.sha1 = Some(sha1.into());
        self
    }

    /// Used when the index publishes no SHA-1.
    pub fn with_sha512(mut self, sha512: impl Into<String>) -> Self {
        self.sha512 = Some(sha512.into());
        self
    }

    pub fn with_size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    fn is_valid(&self) -> bool {
        if !matches!(verify_file(&self.dest, self.sha1.as_deref(), self.size), Verify::Ok) {
            return false;
        }
        match &self.sha512 {
            Some(expected) if self.sha1.is_none() => {
                tntl_store::sha512_file(&self.dest).is_ok_and(|actual| actual.eq_ignore_ascii_case(expected))
            }
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadOutcome {
    AlreadyValid,
    Downloaded,
}

#[derive(Debug, Clone)]
pub struct Http {
    client: reqwest::blocking::Client,
}

impl Http {
    pub fn new() -> Result<Self, SourceError> {
        let builder = reqwest::blocking::Client::builder()
            .user_agent(endpoints::user_agent())
            .connect_timeout(Duration::from_secs(15))
            // No read timeout exists on the blocking client; a total timeout
            // keeps a stalled CDN or proxy from hanging an install forever.
            .timeout(Duration::from_secs(900))
            .dns_resolver(std::sync::Arc::new(PreferIpv4))
            .pool_max_idle_per_host(8);
        let client = crate::network::apply_proxy(builder)?.build()?;
        Ok(Self { client })
    }

    pub fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, SourceError> {
        let body = self.get_bytes(url, 8 * 1024 * 1024)?;
        serde_json::from_slice(&body).map_err(|error| SourceError::Io {
            path: PathBuf::from(url),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        })
    }

    pub fn get_bytes(&self, url: &str, limit: usize) -> Result<Vec<u8>, SourceError> {
        self.get_bytes_with(url, limit, &[])
    }

    /// GET with extra request headers, e.g. an API key.
    pub fn get_bytes_with(&self, url: &str, limit: usize, headers: &[(&str, &str)]) -> Result<Vec<u8>, SourceError> {
        self.execute(url, limit, |client| with_headers(client.get(url), headers))
    }

    /// POST a JSON request body and return the response bytes.
    pub fn post_json(&self, url: &str, body: &str) -> Result<Vec<u8>, SourceError> {
        self.post_json_with(url, body, &[])
    }

    /// POST a JSON request body with extra request headers.
    pub fn post_json_with(&self, url: &str, body: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>, SourceError> {
        self.execute(url, 8 * 1024 * 1024, |client| {
            with_headers(
                client
                    .post(url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.to_string()),
                headers,
            )
        })
    }

    fn execute<F>(&self, url: &str, limit: usize, build: F) -> Result<Vec<u8>, SourceError>
    where
        F: Fn(&reqwest::blocking::Client) -> reqwest::blocking::RequestBuilder,
    {
        let mut last_error = None;
        for attempt in 0..4 {
            if attempt > 0 {
                std::thread::sleep(backoff(attempt));
            }
            match build(&self.client).send() {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        let mut body = Vec::new();
                        let mut limited = response.take(limit as u64 + 1);
                        // A connection can die after the headers; retry that
                        // just like a failed connect instead of failing the
                        // whole install.
                        if let Err(error) = limited.read_to_end(&mut body) {
                            last_error = Some(SourceError::Body(error.to_string()));
                            continue;
                        }
                        if body.len() > limit {
                            return Err(SourceError::Body(format!("{url} is larger than {limit} bytes")));
                        }
                        return Ok(body);
                    }
                    if retryable(status.as_u16()) {
                        last_error = Some(SourceError::Status {
                            status: status.as_u16(),
                            url: url.to_string(),
                        });
                        continue;
                    }
                    return Err(SourceError::Status {
                        status: status.as_u16(),
                        url: url.to_string(),
                    });
                }
                Err(error) => {
                    let retry = error.is_timeout() || error.is_connect() || error.is_request();
                    last_error = Some(SourceError::Network(error));
                    if !retry {
                        break;
                    }
                }
            }
        }
        Err(last_error.unwrap_or(SourceError::Retries {
            attempts: 4,
            url: url.to_string(),
        }))
    }

    /// Download one artifact. Existing files that pass verification are kept.
    pub fn download(
        &self,
        request: &DownloadRequest,
        progress: &mut dyn FnMut(u64),
    ) -> Result<DownloadOutcome, SourceError> {
        crate::cancel::check()?;
        if request.is_valid() {
            return Ok(DownloadOutcome::AlreadyValid);
        }
        if let Some(parent) = request.dest.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        let temp = temp_path(&request.dest);
        let mut last_error = None;
        for attempt in 0..4 {
            if attempt > 0 {
                std::thread::sleep(backoff(attempt));
            }
            crate::cancel::check()?;
            match self.fetch_to_file(&request.url, &temp, progress) {
                Ok(declared) => {
                    if let Some(size) = request.size {
                        let actual = fs::metadata(&temp).map(|meta| meta.len()).unwrap_or_default();
                        if actual != size {
                            let _ = fs::remove_file(&temp);
                            last_error = Some(SourceError::Body(format!(
                                "{} ended at {actual} bytes, expected {size}",
                                request.url
                            )));
                            continue;
                        }
                    } else if let Some(declared) = declared
                        && fs::metadata(&temp).map(|meta| meta.len()).unwrap_or_default() != declared
                    {
                        // Sources that publish no hash often still send a
                        // length; a clean early close would otherwise keep a
                        // truncated file.
                        let _ = fs::remove_file(&temp);
                        last_error = Some(SourceError::Body(format!(
                            "{} ended early, expected {declared} bytes",
                            request.url
                        )));
                        continue;
                    }
                    let actual = tntl_store::sha1_file(&temp).map_err(|error| io_error(&temp, error))?;
                    if let Some(expected) = &request.sha1
                        && !actual.eq_ignore_ascii_case(expected)
                    {
                        let _ = fs::remove_file(&temp);
                        return Err(SourceError::Hash {
                            path: request.dest.clone(),
                            expected: expected.clone(),
                            actual,
                        });
                    }
                    if request.sha1.is_none()
                        && let Some(expected) = &request.sha512
                    {
                        let actual = tntl_store::sha512_file(&temp).map_err(|error| io_error(&temp, error))?;
                        if !actual.eq_ignore_ascii_case(expected) {
                            let _ = fs::remove_file(&temp);
                            return Err(SourceError::Hash {
                                path: request.dest.clone(),
                                expected: expected.clone(),
                                actual,
                            });
                        }
                    }
                    fs::rename(&temp, &request.dest).map_err(|error| io_error(&request.dest, error))?;
                    return Ok(DownloadOutcome::Downloaded);
                }
                Err(error) => {
                    let _ = fs::remove_file(&temp);
                    // A 404 or 403 does not become a 200 on the third try; only
                    // transient statuses and broken connections are retried.
                    let retry = match &error {
                        SourceError::Network(_) | SourceError::Body(_) => true,
                        SourceError::Status { status, .. } => retryable(*status),
                        _ => false,
                    };
                    last_error = Some(error);
                    if !retry {
                        break;
                    }
                }
            }
        }
        Err(last_error.unwrap_or(SourceError::Retries {
            attempts: 4,
            url: request.url.clone(),
        }))
    }

    fn fetch_to_file(&self, url: &str, dest: &Path, progress: &mut dyn FnMut(u64)) -> Result<Option<u64>, SourceError> {
        let mut response = self.client.get(url).send()?;
        let status = response.status();
        if !status.is_success() {
            return Err(SourceError::Status {
                status: status.as_u16(),
                url: url.to_string(),
            });
        }
        let declared = response.content_length();
        let mut file = File::create(dest).map_err(|error| io_error(dest, error))?;
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| SourceError::Body(error.to_string()))?;
            if read == 0 {
                break;
            }
            // The desktop shell can ask for a cancel while a big file streams.
            crate::cancel::check()?;
            file.write_all(&buffer[..read]).map_err(|error| io_error(dest, error))?;
            total += read as u64;
            progress(total);
        }
        file.flush().map_err(|error| io_error(dest, error))?;
        Ok(declared)
    }
}

/// Puts IPv4 addresses before IPv6 ones. A machine whose IPv6 path is only
/// half working would otherwise stall every download on the AAAA record.
#[derive(Debug)]
struct PreferIpv4;

impl reqwest::dns::Resolve for PreferIpv4 {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let result = (host.as_str(), 0u16)
            .to_socket_addrs()
            .map(|addrs| {
                let mut addrs: Vec<SocketAddr> = addrs.collect();
                addrs.sort_by_key(|addr| !addr.is_ipv4());
                Box::new(addrs.into_iter()) as reqwest::dns::Addrs
            })
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>);
        Box::pin(std::future::ready(result))
    }
}

fn retryable(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

fn with_headers(
    mut builder: reqwest::blocking::RequestBuilder,
    headers: &[(&str, &str)],
) -> reqwest::blocking::RequestBuilder {
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder
}

fn backoff(attempt: u32) -> Duration {
    let base = 400u64 << attempt.min(4);
    let jitter = (std::process::id() as u64 * 31 + attempt as u64 * 17) % 250;
    Duration::from_millis(base + jitter)
}

fn temp_path(dest: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    name.push_str(&format!(".{}.{seq}.part", std::process::id()));
    dest.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The cancel flag is process-wide, so the two phases live in one test:
    // a second test would clear the flag under the first one.
    #[test]
    fn cancel_stops_downloads_before_and_during_the_transfer() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let dir = std::env::temp_dir().join(format!("tntl-cancel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let request = DownloadRequest::new("http://127.0.0.1:9/nothing", dir.join("x"));
        let first = crate::cancel::begin();
        crate::cancel::request();
        let error = Http::new().unwrap().download(&request, &mut |_| {}).unwrap_err();
        assert!(matches!(error, SourceError::Cancelled));
        // A second job starting while the first is still live must not clear
        // the cancel: the first one would keep downloading.
        let second = crate::cancel::begin();
        assert!(crate::cancel::is_cancelled());
        drop(first);
        drop(second);
        // With no operation left the flag is gone, so the next one starts clean.
        assert!(!crate::cancel::is_cancelled());
        let _second = crate::cancel::begin();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // The server reports the connection instead of the test sleeping for a
        // fixed time: building the client can take longer than any sleep the
        // test picks, and a cancel that arrives before the connection exists
        // makes the download return without ever reaching `accept`, which used
        // to leave `server.join()` waiting for ever.
        let (connected, connection) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let _ = connected.send(());
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf);
            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n");
            for _ in 0..300 {
                if socket.write_all(&[0u8; 4096]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let request = DownloadRequest::new(format!("http://{addr}/slow"), dir.join("slow.bin"));
        let _third = crate::cancel::begin();
        let worker = std::thread::spawn(move || Http::new().unwrap().download(&request, &mut |_| {}).map(|_| ()));
        connection
            .recv_timeout(Duration::from_secs(10))
            .expect("the download never connected");
        crate::cancel::request();
        let result = worker.join().unwrap();
        assert!(matches!(result, Err(SourceError::Cancelled)), "{result:?}");
        let _fourth = crate::cancel::begin();
        assert!(!dir.join("slow.bin").exists());
        let _ = fs::remove_dir_all(&dir);
        server.join().ok();

        // A cancel left over from a finished install must not turn a later
        // search or skin download into "cancelled".
        drop(_second);
        drop(_third);
        drop(_fourth);
        crate::cancel::request();
        assert!(crate::cancel::check().is_ok());
        let _clean = crate::cancel::begin();
    }
}
