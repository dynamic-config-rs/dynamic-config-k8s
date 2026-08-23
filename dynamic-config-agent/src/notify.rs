//! Telling the application that the file moved.
//!
//! The rename is atomic, so a consumer never sees half a document — but
//! nobody tells it the document changed. nginx, Prometheus and most legacy
//! daemons reload on a signal or an endpoint and on nothing else, and a
//! sidecar that renders perfectly for a process which never re-reads the
//! file has delivered nothing.
//!
//! # Why an endpoint and not a signal
//!
//! Signalling a sibling container needs `shareProcessNamespace: true` on
//! the pod — a pod-wide change to the process boundary between containers,
//! which the webhook will not make on a workload's behalf — plus a
//! dependency for `kill(2)` against a crate that forbids unsafe code, in an
//! image with no shell to exec one from.
//!
//! An endpoint needs none of that. This module is a `TcpStream` and a
//! hand-written request, in the same house style as the metrics server.
//!
//! # Why localhost only
//!
//! An agent that will POST to an arbitrary URL is an SSRF primitive holding
//! a Kubernetes service account and a store credential. The address is
//! checked here as well as at admission, because the two run in different
//! places and only one of them is on the pod.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// How long the whole call may take.
///
/// Short: the file is already correct and already published, so this is a
/// courtesy rather than a step. An application that takes longer than this
/// to accept a reload request is one whose reload is not on this path.
const DEADLINE: Duration = Duration::from_secs(2);

/// A localhost endpoint to POST to after a render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    host: String,
    port: u16,
    path: String,
}

impl Endpoint {
    /// Parses `http://127.0.0.1:8080/-/reload`.
    ///
    /// # Errors
    ///
    /// If the scheme is not `http`, if the host is not a loopback name, or
    /// if there is no port. Every refusal says which.
    pub fn parse(url: &str) -> Result<Self, String> {
        let rest = url.strip_prefix("http://").ok_or_else(|| {
            format!(
                "{url:?}: only `http://` — this is a call to the pod's own \
                 localhost, where TLS has nothing to authenticate"
            )
        })?;

        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        };

        let (host, port) = authority.rsplit_once(':').ok_or_else(|| {
            format!("{url:?}: no port — an application's reload endpoint is a port on localhost")
        })?;

        // By name and by address, and nothing else. Resolving a name here
        // and trusting the answer is how a config agent becomes a way to
        // reach whatever DNS decides to point at.
        if !matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1") {
            return Err(format!(
                "{url:?}: the host is {host:?}, and only localhost is \
                 allowed — an agent that will POST anywhere is a way to \
                 reach anything, holding this pod's credentials"
            ));
        }

        let port: u16 = port
            .parse()
            .map_err(|_| format!("{url:?}: {port:?} is not a port"))?;

        Ok(Self {
            host: host.trim_matches(['[', ']']).to_owned(),
            port,
            path: path.to_owned(),
        })
    }

    /// One POST, bounded, and the status line's code.
    ///
    /// # Errors
    ///
    /// If the connection, the write or the read fails or outlives
    /// [`DEADLINE`]. The caller logs it and carries on: the file is already
    /// published, and a notification that did not arrive has not undone it.
    pub async fn call(&self) -> Result<u16, String> {
        let work = async {
            let mut stream = TcpStream::connect((self.host.as_str(), self.port))
                .await
                .map_err(|error| format!("connecting: {error}"))?;

            // `Connection: close` so the server hangs up and the read below
            // ends without a content-length parser. There is one request on
            // this connection and nothing to keep it alive for.
            let request = format!(
                "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Length: 0\r\n\
                 User-Agent: dynamic-config-agent\r\nConnection: close\r\n\r\n",
                self.path, self.host, self.port
            );

            stream
                .write_all(request.as_bytes())
                .await
                .map_err(|error| format!("writing: {error}"))?;

            let mut response = Vec::new();

            // Bounded by the deadline around this whole future, and by the
            // server closing. A reload endpoint answers in bytes, not
            // megabytes, and nothing here reads the body.
            stream
                .take(8 * 1024)
                .read_to_end(&mut response)
                .await
                .map_err(|error| format!("reading: {error}"))?;

            status_of(&response).ok_or_else(|| "the answer was not an HTTP status line".to_owned())
        };

        tokio::time::timeout(DEADLINE, work)
            .await
            .map_err(|_| format!("no answer within {DEADLINE:?}"))?
    }
}

/// The code out of `HTTP/1.1 200 OK`.
fn status_of(response: &[u8]) -> Option<u16> {
    let line = response.split(|byte| *byte == b'\n').next()?;
    let text = std::str::from_utf8(line).ok()?;

    text.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_localhost_endpoint_parses_into_its_three_parts() {
        let endpoint = Endpoint::parse("http://127.0.0.1:8080/-/reload").expect("well formed");

        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 8080);
        assert_eq!(endpoint.path, "/-/reload");
    }

    #[test]
    fn a_missing_path_is_the_root() {
        assert_eq!(
            Endpoint::parse("http://localhost:9000")
                .expect("well formed")
                .path,
            "/"
        );
    }

    /// The refusal that matters: an agent that will POST anywhere is a way
    /// to reach anything, holding this pod's credentials.
    #[test]
    fn anything_that_is_not_localhost_is_refused() {
        for url in [
            "http://example.com:80/reload",
            "http://10.0.0.5:8080/reload",
            "http://169.254.169.254/latest/meta-data",
            "http://kubernetes.default.svc/api",
        ] {
            let error = Endpoint::parse(url).expect_err(url);

            assert!(
                error.contains("localhost") || error.contains("not a port"),
                "{url}: {error}"
            );
        }
    }

    #[test]
    fn https_is_refused_and_says_why() {
        let error = Endpoint::parse("https://127.0.0.1:8443/reload").expect_err("not http");

        assert!(error.contains("http://"), "{error}");
    }

    #[test]
    fn the_status_line_is_read_and_nothing_else_is() {
        assert_eq!(status_of(b"HTTP/1.1 204 No Content\r\n\r\n"), Some(204));
        assert_eq!(
            status_of(b"HTTP/1.1 500 Internal Server Error\r\n"),
            Some(500)
        );
        assert_eq!(status_of(b"garbage"), None);
    }
}
