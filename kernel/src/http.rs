//! Shared asynchronous HTTPS transport. Reqwest owns HTTP framing, DNS, TLS,
//! and connection reuse; callers own redirect policy, retries and body limits.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::io::StreamReader;

pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(&'a str, String)],
    pub body: &'a [u8],
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Pin<Box<dyn AsyncRead + Send>>,
}

impl Response {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Read at most `cap` bytes, replacing invalid UTF-8. Error pages may be
    /// truncated; structured documents should use a strict body limit instead.
    pub async fn text(self, cap: usize) -> Result<String, String> {
        let mut bytes = Vec::new();
        self.body
            .take(cap as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// DNS, TCP and TLS connection establishment.
    pub connect: Duration,
    /// Sending the request body and waiting for headers, then separately
    /// waiting for the first response body chunk.
    pub first_byte: Duration,
    /// Waiting between body chunks; never a deadline for the whole stream.
    pub idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(30),
            first_byte: Duration::from_secs(120),
            idle: Duration::from_secs(60),
        }
    }
}

/// Shared TLS roots and provider on every platform, including Android.
#[must_use]
pub fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("ring protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
        )
    })
    .clone()
}

/// Authenticated transports never follow redirects or retry requests. In
/// particular a model POST and a conditional object write must happen once.
fn client_builder(connect: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .use_preconfigured_tls((*tls_config()).clone())
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(connect)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
}

fn client(connect: Duration) -> Result<reqwest::Client, String> {
    client_builder(connect).build().map_err(|e| e.to_string())
}

pub async fn send(req: &Request<'_>) -> Result<Response, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    let client = CLIENT
        .get_or_init(|| client(Timeouts::default().connect))
        .as_ref()
        .map_err(Clone::clone)?;
    send_using(client, req, Timeouts::default()).await
}

pub async fn send_with(req: &Request<'_>, timeouts: Timeouts) -> Result<Response, String> {
    send_using(&client(timeouts.connect)?, req, timeouts).await
}

async fn send_using(
    client: &reqwest::Client,
    req: &Request<'_>,
    timeouts: Timeouts,
) -> Result<Response, String> {
    let method = reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|e| e.to_string())?;
    let mut request = client.request(method, req.url).body(req.body.to_vec());
    for (key, value) in req.headers {
        request = request.header(*key, value);
    }
    let response = tokio::time::timeout(timeouts.connect + timeouts.first_byte, request.send())
        .await
        .map_err(|_| "HTTP response headers timed out".to_string())?
        .map_err(|e| e.without_url().to_string())?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(key, value)| {
            (
                key.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let chunks =
        futures_util::stream::unfold((response, true), move |(mut response, first)| async move {
            let budget = if first {
                timeouts.first_byte
            } else {
                timeouts.idle
            };
            let chunk = match tokio::time::timeout(budget, response.chunk()).await {
                Ok(Ok(Some(bytes))) => Ok(bytes),
                Ok(Ok(None)) => return None,
                Ok(Err(error)) => Err(io::Error::other(error.without_url())),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("no response bytes for {} s", budget.as_secs_f64()),
                )),
            };
            Some((chunk, (response, false)))
        });
    Ok(Response {
        status,
        headers,
        body: Box::pin(StreamReader::new(Box::pin(chunks))),
    })
}

/// Strict bound for a structured response, checked while streaming, including
/// after decompression. Content-Length alone cannot enforce this bound.
pub async fn bounded_bytes(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| e.without_url().to_string())?
    {
        if chunk.len() > cap.saturating_sub(bytes.len()) {
            return Err(format!("response exceeds {cap} bytes"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    async fn request_head(socket: &mut tokio::net::TcpStream) {
        let mut head = Vec::new();
        while !head.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let mut bytes = [0; 512];
            let received = socket.read(&mut bytes).await.unwrap();
            assert!(received > 0, "request ended before its headers");
            head.extend_from_slice(&bytes[..received]);
            assert!(head.len() <= 16 * 1024, "test request headers are bounded");
        }
    }

    async fn server(wire: &'static [u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            request_head(&mut socket).await;
            socket.write_all(wire).await.unwrap();
        });
        url
    }

    #[tokio::test]
    async fn framed_bodies_and_header_lookup() {
        let url = server(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-Test: yes\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").await;
        let response = send_using(
            &reqwest::Client::new(),
            &Request {
                method: "GET",
                url: &url,
                headers: &[],
                body: &[],
            },
            Timeouts::default(),
        )
        .await
        .unwrap();
        assert_eq!(response.header("X-TEST"), Some("yes"));
        assert_eq!(response.text(4).await.unwrap(), "abcd");
    }

    #[tokio::test]
    async fn structured_body_limit_is_strict() {
        let url = server(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nabcde").await;
        let response = reqwest::get(url).await.unwrap();
        assert!(bounded_bytes(response, 4)
            .await
            .unwrap_err()
            .contains("exceeds"));
    }

    #[tokio::test]
    async fn authenticated_transport_rejects_plaintext() {
        assert!(send(&Request {
            method: "POST",
            url: "http://127.0.0.1:1",
            headers: &[],
            body: b"secret"
        })
        .await
        .is_err());
    }

    #[tokio::test]
    async fn authenticated_requests_do_not_follow_redirects() {
        let url = server(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Length: 0\r\n\r\n").await;
        let client = client_builder(Duration::from_secs(1))
            .https_only(false)
            .build()
            .unwrap();
        let response = send_using(
            &client,
            &Request {
                method: "POST",
                url: &url,
                headers: &[("authorization", "Bearer secret".into())],
                body: b"private",
            },
            Timeouts::default(),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 307);
    }

    #[tokio::test]
    async fn body_budgets_distinguish_thinking_from_a_stalled_stream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            request_head(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(80)).await;
            socket.write_all(b"a").await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let timeouts = Timeouts {
            connect: Duration::from_secs(1),
            first_byte: Duration::from_secs(1),
            idle: Duration::from_millis(30),
        };
        let mut response = send_using(
            &reqwest::Client::new(),
            &Request {
                method: "GET",
                url: &url,
                headers: &[],
                body: &[],
            },
            timeouts,
        )
        .await
        .unwrap();
        let mut byte = [0];
        response.body.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, [b'a']);
        assert_eq!(
            response
                .body
                .read_exact(&mut byte)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        task.abort();
    }
}
