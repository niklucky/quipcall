//! Application HTTP independent of call lifetimes, usable from any UI executor.
pub use futures_util::future::join;
pub use reqwest::Url;
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

#[derive(Debug)]
pub struct RequestError {
    pub status: u16,
    pub message: String,
}
impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RequestError {}

/// Retryable polling failures; credential/proof errors and malformed successes
/// remain terminal. The caller owns its deadline, backoff and cancellation.
pub fn transient(error: &anyhow::Error) -> bool {
    if let Some(e) = error.downcast_ref::<RequestError>() {
        return matches!(e.status, 408 | 429 | 500..=599);
    }
    error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(|e| e.is_connect() || e.is_timeout() || e.is_body() || e.is_request())
}

fn client() -> anyhow::Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<Result<reqwest::Client, reqwest::Error>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub fn request(
    server: &str,
    token: Option<&str>,
    path: &str,
    body: Option<Value>,
    method: &str,
) -> impl Future<Output = anyhow::Result<Value>> + Send + 'static + use<> {
    request_with_retry(server, token, path, body, method, false)
}

pub(crate) fn request_with_retry(
    server: &str,
    token: Option<&str>,
    path: &str,
    body: Option<Value>,
    method: &str,
    retry: bool,
) -> impl Future<Output = anyhow::Result<Value>> + Send + 'static + use<> {
    let url = format!("{}/api{path}", server.trim_end_matches('/'));
    let token = token.map(str::to_owned);
    let method = method.to_owned();
    async move {
        crate::runtime()?
            .spawn(async move {
                let client = client()?;
                let mut attempt = 1;
                let response = loop {
                    let mut req = client
                        .request(method.parse()?, &url)
                        .header("X-Quipcall", "1");
                    if let Some(token) = &token {
                        req = req.bearer_auth(token);
                    }
                    if let Some(body) = &body {
                        req = req.json(body);
                    }
                    match req.send().await {
                        Ok(response) => break response,
                        Err(e) if retry && attempt < 3 && (e.is_connect() || e.is_timeout()) => {
                            tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                            attempt += 1;
                        }
                        Err(e) => return Err(e.into()),
                    }
                };
                let status = response.status();
                if !status.is_success() {
                    let value: Value = response.json().await.unwrap_or_default();
                    return Err(RequestError {
                        status: status.as_u16(),
                        message: value["error"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("Request failed ({status})")),
                    }
                    .into());
                }
                Ok(response.json().await?)
            })
            .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn polling_recovers_after_a_server_drops_the_connection() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let responder = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut headers = Vec::new();
                let mut byte = [0];
                while !headers.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                if attempt == 1 {
                    let body = r#"{"pending":true}"#;
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                }
            }
        });
        crate::runtime().unwrap().block_on(async {
            let error = request(&server, None, "/auth/desktop/poll", None, "POST")
                .await
                .unwrap_err();
            assert!(transient(&error));
            assert_eq!(
                request(&server, None, "/auth/desktop/poll", None, "POST")
                    .await
                    .unwrap()["pending"],
                true
            );
        });
        responder.join().unwrap();
    }

    #[test]
    fn polling_retries_transport_and_server_errors_only() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(transient(
                &RequestError {
                    status,
                    message: "temporary".into()
                }
                .into()
            ));
        }
        for status in [400, 401, 403, 404, 410] {
            assert!(!transient(
                &RequestError {
                    status,
                    message: "terminal".into()
                }
                .into()
            ));
        }
        assert!(!transient(&anyhow::anyhow!("Malformed response")));
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", socket.local_addr().unwrap());
        drop(socket);
        let error = crate::runtime()
            .unwrap()
            .block_on(request(&server, None, "/auth/desktop/poll", None, "POST"))
            .unwrap_err();
        assert!(transient(&error));
    }

    #[test]
    fn engines_have_separate_runtimes_and_http_reuses_its_client() {
        let (a, _) = crate::Engine::new().unwrap();
        let (b, _) = crate::Engine::new().unwrap();
        assert!(!std::ptr::eq(a.rt(), b.rt()));
        assert!(!std::ptr::eq(a.rt(), crate::runtime().unwrap()));
        assert!(std::ptr::eq(client().unwrap(), client().unwrap()));
    }

    #[test]
    fn admission_waits_for_permission_and_preserves_an_expired_session_status() {
        use futures_util::FutureExt as _;
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let (ready, permission) = async_channel::bounded::<()>(1);
        crate::runtime().unwrap().block_on(async {
            let permission = async move { permission.recv().await?; anyhow::Ok(()) };
            let request = request_with_retry(&server, Some("expired-session"), "/join", Some(serde_json::json!({"room":"test"})), "POST", true);
            let mut joining = std::pin::pin!(crate::after_permission(permission, request));
            assert!(joining.as_mut().now_or_never().is_none());
            assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            // No admission request exists while the prompt remains unanswered,
            // irrespective of how long the user leaves it open.
            listener.set_nonblocking(false).unwrap();
            let responder = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut bytes = Vec::new(); let mut byte = [0];
                while !bytes.ends_with(b"\r\n\r\n") { stream.read_exact(&mut byte).unwrap(); bytes.push(byte[0]); }
                let headers = String::from_utf8(bytes).unwrap().to_lowercase();
                assert!(headers.starts_with("post /api/join "));
                assert!(headers.contains("authorization: bearer expired-session"));
                let len: usize = headers.lines().find_map(|l| l.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                stream.read_exact(&mut vec![0; len]).unwrap();
                let body = r#"{"error":"Sign in to continue"}"#;
                write!(stream, "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            ready.send(()).await.unwrap();
            let error = joining.await.unwrap_err();
            let error = error.downcast_ref::<RequestError>().unwrap();
            assert_eq!(error.status, 401);
            assert_eq!(error.message, "Sign in to continue");
            responder.join().unwrap();
        });
    }
}
