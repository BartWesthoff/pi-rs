//! A small scriptable HTTP/1.1 server for offline provider tests (one request per connection).
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    /// Path and query.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn header_count(&self, name: &str) -> usize {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .count()
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
    }
    pub fn path_only(&self) -> &str {
        self.path.split('?').next().unwrap_or("")
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Body chunks; more than one chunk is sent with chunked transfer encoding.
    pub chunks: Vec<Vec<u8>>,
    /// Delay before the response head is written.
    pub delay_ms: u64,
    /// Delay between body chunks.
    pub chunk_delay_ms: u64,
    /// Close the connection after the body chunks without the terminating zero-length chunk:
    /// the server drops the connection mid-body.
    pub truncate: bool,
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            chunks: vec![body.into()],
            delay_ms: 0,
            chunk_delay_ms: 0,
            truncate: false,
        }
    }
    pub fn json(status: u16, body: serde_json::Value) -> Self {
        Self::new(status, "application/json", body.to_string())
    }
    pub fn text(status: u16, body: &str) -> Self {
        Self::new(status, "text/plain", body.to_string())
    }
    pub fn sse(body: &str) -> Self {
        Self::new(200, "text/event-stream", body.to_string())
    }
    pub fn chunked(status: u16, content_type: &str, chunks: Vec<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            chunks,
            delay_ms: 0,
            chunk_delay_ms: 0,
            truncate: false,
        }
    }
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    pub fn with_delay(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }
    pub fn with_chunk_delay(mut self, ms: u64) -> Self {
        self.chunk_delay_ms = ms;
        self
    }
    /// Drop the connection after the last chunk instead of ending the chunked body.
    pub fn truncated(mut self) -> Self {
        self.truncate = true;
        self
    }
}

type Handler = Arc<dyn Fn(&Request, usize) -> Response + Send + Sync>;

pub struct MockServer {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Request>>>,
}

impl MockServer {
    /// Serve requests with `handler(request, index)`.
    pub async fn start(
        handler: impl Fn(&Request, usize) -> Response + Send + Sync + 'static,
    ) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();
        let handler: Handler = Arc::new(handler);
        let captured = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let captured = captured.clone();
                tokio::spawn(async move {
                    let _ = serve_connection(socket, handler, captured).await;
                });
            }
        });
        MockServer { url, requests }
    }

    /// Serve the given responses in order; later requests get the last one.
    pub async fn sequence(responses: Vec<Response>) -> MockServer {
        MockServer::start(move |_, index| responses[index.min(responses.len() - 1)].clone()).await
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().clone()
    }

    pub fn request(&self, index: usize) -> Request {
        self.requests.lock()[index].clone()
    }
}

async fn serve_connection(
    mut socket: TcpStream,
    handler: Handler,
    captured: Arc<Mutex<Vec<Request>>>,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16384];
    let header_end = loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    let content_length = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let request = Request {
        method,
        path,
        headers,
        body,
    };
    let index = {
        let mut guard = captured.lock();
        guard.push(request.clone());
        guard.len() - 1
    };
    let response = handler(&request, index);
    if response.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(response.delay_ms)).await;
    }
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nconnection: close\r\n",
        response.status
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if response.chunks.len() == 1 && !response.truncate {
        head.push_str(&format!(
            "content-length: {}\r\n\r\n",
            response.chunks[0].len()
        ));
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(&response.chunks[0]).await?;
    } else {
        head.push_str("transfer-encoding: chunked\r\n\r\n");
        socket.write_all(head.as_bytes()).await?;
        for chunk in &response.chunks {
            if response.chunk_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(response.chunk_delay_ms)).await;
            }
            socket
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await?;
            socket.write_all(chunk).await?;
            socket.write_all(b"\r\n").await?;
            socket.flush().await?;
        }
        if response.truncate {
            // Dropping the socket closes the connection with the chunked body unfinished.
            return Ok(());
        }
        socket.write_all(b"0\r\n\r\n").await?;
    }
    socket.flush().await?;
    let _ = socket.shutdown().await;
    Ok(())
}
