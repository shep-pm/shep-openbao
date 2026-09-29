//! An OpenBao stand-in for unit tests: plain HTTP on a loopback port,
//! answering each route from a script and recording every request.
//!
//! Hand-written rather than a mock-server crate because it needs three
//! things, all small: read one request, look up its answer, and close the
//! connection. `Connection: close` on every answer keeps it to one request
//! per connection, so there is no keep-alive to parse.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use reqwest::Url;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// One request as the fake received it. Header names are lowercased.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

type Routes = HashMap<(String, String), (u16, String)>;

/// A running fake. The listening task stops when the test's runtime does.
#[derive(Debug)]
pub struct FakeBao {
    address: Url,
    routes: Arc<Mutex<Routes>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeBao {
    /// Starts listening on `127.0.0.1` on a port the OS picks.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let port = listener.local_addr().expect("a bound address").port();
        let routes: Arc<Mutex<Routes>> = Arc::default();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (task_routes, task_seen) = (Arc::clone(&routes), Arc::clone(&seen));
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (routes, seen) = (Arc::clone(&task_routes), Arc::clone(&task_seen));
                tokio::spawn(serve(stream, routes, seen));
            }
        });
        let address = Url::parse(&format!("http://127.0.0.1:{port}")).expect("a URL");
        Self {
            address,
            routes,
            seen,
        }
    }

    /// Where it listens, in the form `address` in `[openbao]` takes.
    pub fn address(&self) -> Url {
        self.address.clone()
    }

    /// Answers `method path` with `status` and `body` from now on. An
    /// unscripted route answers 404.
    pub fn answer(&self, method: &str, path: &str, status: u16, body: &str) {
        lock(&self.routes).insert(
            (method.to_string(), path.to_string()),
            (status, body.to_string()),
        );
    }

    /// Scripts a successful AppRole login handing out `token` for `ttl`
    /// seconds.
    pub fn login_ok(&self, token: &str, ttl: u64) {
        self.answer(
            "POST",
            "/v1/auth/approle/login",
            200,
            &format!(r#"{{"auth": {{"client_token": "{token}", "lease_duration": {ttl}, "renewable": true}}}}"#),
        );
    }

    /// Scripts a KV v2 read of `mount/path` answering with `data`, a JSON
    /// object.
    pub fn kv(&self, mount: &str, path: &str, data: &str) {
        self.answer(
            "GET",
            &format!("/v1/{mount}/data/{path}"),
            200,
            &format!(r#"{{"data": {{"data": {data}, "metadata": {{"version": 1}}}}}}"#),
        );
    }

    /// Every request received so far, oldest first.
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.seen).clone()
    }

    /// How many requests have been for `path`.
    pub fn count(&self, path: &str) -> usize {
        lock(&self.seen).iter().filter(|s| s.path == path).count()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn serve(mut stream: TcpStream, routes: Arc<Mutex<Routes>>, seen: Arc<Mutex<Vec<Seen>>>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let (status, body) = lock(&routes)
        .get(&(request.method.clone(), request.path.clone()))
        .cloned()
        .unwrap_or((404, r#"{"errors": []}"#.to_string()));
    lock(&seen).push(request);
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let head_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_string();
    let path = request_line.next()?.to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(Seen {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}
