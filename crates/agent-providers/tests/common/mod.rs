#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    pub fn json_body(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }

    pub fn header(&self, name: &str) -> Option<&String> {
        let needle = name.to_ascii_lowercase();
        self.headers.get(&needle)
    }
}

#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub status_text: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub chunks: Vec<Vec<u8>>,
    pub delay_between_chunks_ms: Option<u64>,
    pub drop_connection_after_chunk: Option<usize>,
}

impl MockResponse {
    pub fn sse_chunks(chunks: Vec<String>) -> Self {
        let byte_chunks = chunks.into_iter().map(|s| s.into_bytes()).collect();
        Self {
            status: 200,
            status_text: "OK",
            headers: vec![
                ("Content-Type", "text/event-stream".into()),
                ("Cache-Control", "no-cache".into()),
                ("Transfer-Encoding", "chunked".into()),
            ],
            chunks: byte_chunks,
            delay_between_chunks_ms: None,
            drop_connection_after_chunk: None,
        }
    }

    pub fn sse_with_disconnect(chunks: Vec<String>, drop_after_chunk: usize) -> Self {
        let mut r = Self::sse_chunks(chunks);
        r.drop_connection_after_chunk = Some(drop_after_chunk);
        r
    }

    pub fn json(status: u16, status_text: &'static str, body: serde_json::Value) -> Self {
        let body_bytes = serde_json::to_vec(&body).unwrap();
        Self {
            status,
            status_text,
            headers: vec![
                ("Content-Type", "application/json".into()),
                ("Content-Length", body_bytes.len().to_string()),
                ("Connection", "close".into()),
            ],
            chunks: vec![body_bytes],
            delay_between_chunks_ms: None,
            drop_connection_after_chunk: None,
        }
    }

    pub fn error_with_retry_after(status: u16, retry_after: u32, body: serde_json::Value) -> Self {
        let body_bytes = serde_json::to_vec(&body).unwrap();
        Self {
            status,
            status_text: "Too Many Requests",
            headers: vec![
                ("Content-Type", "application/json".into()),
                ("Retry-After", retry_after.to_string()),
                ("Content-Length", body_bytes.len().to_string()),
                ("Connection", "close".into()),
            ],
            chunks: vec![body_bytes],
            delay_between_chunks_ms: None,
            drop_connection_after_chunk: None,
        }
    }

    pub fn status_only(status: u16, status_text: &'static str, body: &str) -> Self {
        let body_bytes = body.as_bytes().to_vec();
        Self {
            status,
            status_text,
            headers: vec![
                ("Content-Type", "text/plain".into()),
                ("Content-Length", body_bytes.len().to_string()),
                ("Connection", "close".into()),
            ],
            chunks: vec![body_bytes],
            delay_between_chunks_ms: None,
            drop_connection_after_chunk: None,
        }
    }
}

pub struct MockServer {
    addr: SocketAddr,
    recorded_requests: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl MockServer {
    pub async fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");

        let recorded_requests = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let reqs_clone = Arc::clone(&recorded_requests);
        let queue_clone = Arc::clone(&queue);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    accept_res = listener.accept() => {
                        let (mut socket, _) = match accept_res {
                            Ok(conn) => conn,
                            Err(_) => break,
                        };

                        let reqs = Arc::clone(&reqs_clone);
                        let q = Arc::clone(&queue_clone);

                        tokio::spawn(async move {
                            let mut buf = Vec::new();
                            let mut tmp = [0u8; 4096];
                            let header_end_pos;

                            // Read until headers end
                            loop {
                                let n = match socket.read(&mut tmp).await {
                                    Ok(0) => return,
                                    Ok(n) => n,
                                    Err(_) => return,
                                };
                                buf.extend_from_slice(&tmp[..n]);

                                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                    header_end_pos = pos + 4;
                                    break;
                                }
                            }

                            // Parse headers
                            let header_str = String::from_utf8_lossy(&buf[..header_end_pos]);
                            let mut lines = header_str.split("\r\n");
                            let req_line = lines.next().unwrap_or("");
                            let mut parts = req_line.split_whitespace();
                            let method = parts.next().unwrap_or("GET").to_string();
                            let path = parts.next().unwrap_or("/").to_string();

                            let mut headers = HashMap::new();
                            let mut content_length: usize = 0;

                            for line in lines {
                                if line.is_empty() {
                                    break;
                                }
                                if let Some((k, v)) = line.split_once(':') {
                                    let key_clean = k.trim().to_ascii_lowercase();
                                    let val_clean = v.trim().to_string();
                                    if key_clean == "content-length" {
                                        content_length = val_clean.parse::<usize>().unwrap_or(0);
                                    }
                                    headers.insert(key_clean, val_clean);
                                }
                            }

                            // Read remaining body if content-length specified
                            let mut body = buf[header_end_pos..].to_vec();
                            while body.len() < content_length {
                                let n = match socket.read(&mut tmp).await {
                                    Ok(0) => break,
                                    Ok(n) => n,
                                    Err(_) => break,
                                };
                                body.extend_from_slice(&tmp[..n]);
                            }

                            reqs.lock().unwrap().push(RecordedRequest {
                                method,
                                path,
                                headers,
                                body,
                            });

                            let resp_opt = q.lock().unwrap().pop_front();
                            let resp = match resp_opt {
                                Some(r) => r,
                                None => MockResponse::status_only(404, "Not Found", "No response queued"),
                            };

                            let mut header_buf = format!(
                                "HTTP/1.1 {} {}\r\n",
                                resp.status, resp.status_text
                            );
                            for (k, v) in &resp.headers {
                                header_buf.push_str(&format!("{k}: {v}\r\n"));
                            }
                            header_buf.push_str("\r\n");

                            if socket.write_all(header_buf.as_bytes()).await.is_err() {
                                return;
                            }
                            let _ = socket.flush().await;

                            let is_chunked = resp.headers.iter().any(|(k, v)| {
                                k.eq_ignore_ascii_case("transfer-encoding")
                                    && v.eq_ignore_ascii_case("chunked")
                            });

                            for (idx, chunk) in resp.chunks.iter().enumerate() {
                                if let Some(drop_idx) = resp.drop_connection_after_chunk {
                                    if idx >= drop_idx {
                                        if is_chunked {
                                            // Write chunk header declaring full chunk length, but only write partial bytes
                                            let chunk_hdr = format!("{:X}\r\n", chunk.len());
                                            let _ = socket.write_all(chunk_hdr.as_bytes()).await;
                                            let partial_len = chunk.len() / 2;
                                            let _ = socket.write_all(&chunk[..partial_len]).await;
                                            let _ = socket.flush().await;
                                        }
                                        let _ = socket.shutdown().await;
                                        return;
                                    }
                                }

                                if is_chunked {
                                    let chunk_hdr = format!("{:X}\r\n", chunk.len());
                                    if socket.write_all(chunk_hdr.as_bytes()).await.is_err() {
                                        return;
                                    }
                                    if socket.write_all(chunk).await.is_err() {
                                        return;
                                    }
                                    if socket.write_all(b"\r\n").await.is_err() {
                                        return;
                                    }
                                } else {
                                    if socket.write_all(chunk).await.is_err() {
                                        return;
                                    }
                                }
                                let _ = socket.flush().await;

                                if let Some(delay_ms) = resp.delay_between_chunks_ms {
                                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                                }
                            }

                            if is_chunked && resp.drop_connection_after_chunk.is_none() {
                                let _ = socket.write_all(b"0\r\n\r\n").await;
                                let _ = socket.flush().await;
                            }
                        });
                    }
                }
            }
        });

        Self {
            addr,
            recorded_requests,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.recorded_requests.lock().unwrap().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}
