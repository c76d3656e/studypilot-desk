//! The desktop application's only local HTTP host.
//!
//! `start` and `LocalApiHandle::stop` are the complete public interface.  The
//! implementation owns HTTP authentication, Actix-Web lifecycle and the
//! private Python protocol.  Python remains a domain adapter during the Rust
//! migration, but it never binds a TCP port or exposes an HTTP interface.

use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_REQUEST_BYTES: usize = 96 * 1024 * 1024;
const SESSION_HEADER: &str = "x-studypilot-session";

/// The command used to start the private Python domain adapter.
#[derive(Clone)]
pub struct PythonWorkerConfig {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
    pub data_directory: PathBuf,
    pub session_token: String,
}

/// Configuration for the Rust-owned local HTTP host.
#[derive(Clone)]
pub struct LocalApiConfig {
    pub port: u16,
    pub public_session_token: String,
    pub python_worker: PythonWorkerConfig,
}

/// A request/response seam for future Rust domain modules and Python adapters.
///
/// The local HTTP host has one adapter today.  It deliberately accepts a
/// protocol-neutral request so a Rust implementation can replace a Python
/// route group without changing the renderer or HTTP lifecycle code.
pub trait RouteAdapter: Send + Sync {
    fn dispatch(&self, request: AdapterRequest) -> Result<AdapterResponse, String>;
}

#[derive(Clone)]
pub struct AdapterRequest {
    pub method: String,
    pub path_and_query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct AdapterResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct LocalApiHandle {
    server: actix_web::dev::ServerHandle,
    thread: Option<JoinHandle<()>>,
    worker: PythonWorker,
}

impl LocalApiHandle {
    pub fn stop(&mut self) {
        let server = self.server.clone();
        actix_web::rt::System::new().block_on(server.stop(true));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.worker.stop();
    }
}

impl Drop for LocalApiHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone)]
struct LocalApiState {
    public_session_token: String,
    worker_session_token: String,
    adapter: Arc<dyn RouteAdapter>,
}

/// Starts Actix-Web on loopback and starts the private non-HTTP Python worker.
pub fn start(config: LocalApiConfig) -> Result<LocalApiHandle, String> {
    let worker = PythonWorker::start(config.python_worker)?;
    let state = LocalApiState {
        public_session_token: config.public_session_token,
        worker_session_token: worker.session_token(),
        adapter: Arc::new(worker.clone()),
    };
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (server_tx, server_rx) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name("studypilot-actix-api".into())
        .spawn(move || {
            let system = actix_web::rt::System::new();
            system.block_on(async move {
                let server = match HttpServer::new(move || {
                    App::new()
                        .app_data(web::Data::new(state.clone()))
                        .app_data(web::PayloadConfig::new(MAX_REQUEST_BYTES))
                        .default_service(web::route().to(dispatch))
                })
                .disable_signals()
                .bind(("127.0.0.1", config.port))
                {
                    Ok(server) => server.run(),
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!("无法监听 Rust Actix API：{error}")));
                        return;
                    }
                };
                let _ = server_tx.send(server.handle());
                let _ = ready_tx.send(Ok(()));
                let _ = server.await;
            });
        })
        .map_err(|error| format!("无法启动 Rust Actix API：{error}"))?;

    match ready_rx.recv_timeout(Duration::from_secs(8)) {
        Ok(Ok(())) => match server_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(server) => Ok(LocalApiHandle {
                server,
                thread: Some(thread),
                worker,
            }),
            Err(error) => {
                worker.stop();
                let _ = thread.join();
                Err(format!("无法取得 Rust Actix API 控制句柄：{error}"))
            }
        },
        Ok(Err(error)) => {
            worker.stop();
            let _ = thread.join();
            Err(error)
        }
        Err(error) => {
            worker.stop();
            let _ = thread.join();
            Err(format!("Rust Actix API 启动超时：{error}"))
        }
    }
}

async fn dispatch(
    request: HttpRequest,
    body: web::Bytes,
    state: web::Data<LocalApiState>,
) -> HttpResponse {
    if body.len() > MAX_REQUEST_BYTES {
        return error_response(413, "PAYLOAD_TOO_LARGE", "请求超过本地 API 限制");
    }
    let is_health = request.path() == "/api/health";
    if !is_health && !session_is_valid(request.headers(), &state.public_session_token) {
        return error_response(401, "UNAUTHORIZED", "本地会话令牌无效");
    }

    let mut headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str();
            if is_hop_by_hop(name) || name.eq_ignore_ascii_case(SESSION_HEADER) {
                return None;
            }
            value
                .to_str()
                .ok()
                .map(|value| (name.to_owned(), value.to_owned()))
        })
        .collect();
    // The public token never reaches Python.  This private token is only used
    // as defence in depth for the in-process protocol adapter.
    headers.push((
        SESSION_HEADER.to_owned(),
        state.worker_session_token.clone(),
    ));
    let target = AdapterRequest {
        method: request.method().as_str().to_owned(),
        path_and_query: request
            .uri()
            .path_and_query()
            .map(|value| value.as_str().to_owned())
            .unwrap_or_else(|| "/".to_owned()),
        headers,
        body: body.to_vec(),
    };
    let adapter = state.adapter.clone();
    match web::block(move || adapter.dispatch(target)).await {
        Ok(Ok(response)) => adapter_response(response),
        Ok(Err(error)) => error_response(502, "PYTHON_ADAPTER_UNAVAILABLE", &error),
        Err(error) => error_response(500, "LOCAL_API_ERROR", &error.to_string()),
    }
}

fn adapter_response(response: AdapterResponse) -> HttpResponse {
    let status = actix_web::http::StatusCode::from_u16(response.status)
        .unwrap_or(actix_web::http::StatusCode::BAD_GATEWAY);
    let mut builder = HttpResponse::build(status);
    for (name, value) in response.headers {
        if is_hop_by_hop(&name) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            actix_web::http::header::HeaderName::try_from(name),
            actix_web::http::header::HeaderValue::try_from(value),
        ) {
            builder.append_header((name, value));
        }
    }
    builder.body(response.body)
}

fn error_response(status: u16, code: &str, message: &str) -> HttpResponse {
    HttpResponse::build(
        actix_web::http::StatusCode::from_u16(status)
            .unwrap_or(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR),
    )
    .content_type("application/json; charset=utf-8")
    .body(format!(
        r#"{{"error":{{"code":"{code}","message":"{message}"}}}}"#
    ))
}

fn session_is_valid(headers: &actix_web::http::header::HeaderMap, expected: &str) -> bool {
    headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|provided| provided == expected)
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
    )
}

#[derive(Clone)]
struct PythonWorker {
    inner: Arc<PythonWorkerInner>,
}

struct PythonWorkerInner {
    session_token: String,
    stdin: Mutex<ChildStdin>,
    child: Mutex<Option<Child>>,
    pending: Mutex<HashMap<u64, mpsc::SyncSender<Result<WireResponse, String>>>>,
    next_id: AtomicU64,
}

#[derive(Serialize)]
struct WireRequest {
    id: u64,
    method: String,
    path_and_query: String,
    headers: Vec<(String, String)>,
    body_base64: String,
}

#[derive(Deserialize)]
struct WireResponse {
    id: u64,
    status: u16,
    headers: Vec<(String, String)>,
    body_base64: String,
}

impl PythonWorker {
    fn start(config: PythonWorkerConfig) -> Result<Self, String> {
        let mut child = Command::new(&config.program)
            .args(&config.arguments)
            .current_dir(&config.working_directory)
            .env("PYTHONIOENCODING", "utf-8")
            .env("STUDYPILOT_DATA_DIR", &config.data_directory)
            .env("STUDYPILOT_SESSION_TOKEN", &config.session_token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("无法启动 Python 领域 Worker：{error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Python 领域 Worker 没有标准输入".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Python 领域 Worker 没有标准输出".to_string())?;
        let inner = Arc::new(PythonWorkerInner {
            session_token: config.session_token,
            stdin: Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let reader_inner = inner.clone();
        thread::Builder::new()
            .name("studypilot-python-worker-protocol".into())
            .spawn(move || read_worker_responses(stdout, reader_inner, ready_tx))
            .map_err(|error| format!("无法读取 Python 领域 Worker：{error}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(12)) {
            Ok(Ok(())) => Ok(Self { inner }),
            Ok(Err(error)) => {
                let worker = Self { inner };
                worker.stop();
                Err(error)
            }
            Err(error) => {
                let worker = Self { inner };
                worker.stop();
                Err(format!("Python 领域 Worker 启动超时：{error}"))
            }
        }
    }

    fn session_token(&self) -> String {
        self.inner.session_token.clone()
    }

    fn stop(&self) {
        if let Ok(mut child) = self.inner.child.lock() {
            if let Some(process) = child.as_mut() {
                let _ = process.kill();
                let _ = process.wait();
            }
            *child = None;
        }
    }
}

impl RouteAdapter for PythonWorker {
    fn dispatch(&self, request: AdapterRequest) -> Result<AdapterResponse, String> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);
        self.inner
            .pending
            .lock()
            .map_err(|_| "Python Worker 请求队列损坏".to_string())?
            .insert(id, sender);
        let wire = WireRequest {
            id,
            method: request.method,
            path_and_query: request.path_and_query,
            headers: request.headers,
            body_base64: BASE64.encode(request.body),
        };
        let encoded = serde_json::to_string(&wire)
            .map_err(|error| format!("无法编码 Python Worker 请求：{error}"))?;
        let write_result = (|| {
            let mut stdin = self
                .inner
                .stdin
                .lock()
                .map_err(|_| std::io::Error::other("Python Worker 输入已关闭"))?;
            stdin.write_all(encoded.as_bytes())?;
            stdin.write_all(b"\n")?;
            stdin.flush()
        })();
        if let Err(error) = write_result {
            let _ = self
                .inner
                .pending
                .lock()
                .map(|mut pending| pending.remove(&id));
            return Err(format!("无法向 Python Worker 发送请求：{error}"));
        }
        match receiver.recv_timeout(Duration::from_secs(600)) {
            Ok(Ok(response)) => Ok(AdapterResponse {
                status: response.status,
                headers: response.headers,
                body: BASE64
                    .decode(response.body_base64)
                    .map_err(|error| format!("Python Worker 返回了无效内容：{error}"))?,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                let _ = self
                    .inner
                    .pending
                    .lock()
                    .map(|mut pending| pending.remove(&id));
                Err("Python Worker 响应超时".to_string())
            }
        }
    }
}

fn read_worker_responses(
    stdout: impl std::io::Read,
    inner: Arc<PythonWorkerInner>,
    ready_tx: mpsc::SyncSender<Result<(), String>>,
) {
    let reader = BufReader::new(stdout);
    let mut ready = false;
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                if !ready {
                    let _ = ready_tx.send(Err(format!("读取 Python Worker 失败：{error}")));
                }
                break;
            }
        };
        if !ready {
            ready = true;
            if line.trim() == r#"{"kind":"ready"}"# {
                let _ = ready_tx.send(Ok(()));
                continue;
            }
            let _ = ready_tx.send(Err(format!("Python Worker 未就绪：{line}")));
            break;
        }
        if let Ok(response) = serde_json::from_str::<WireResponse>(&line) {
            if let Ok(mut pending) = inner.pending.lock() {
                if let Some(sender) = pending.remove(&response.id) {
                    let _ = sender.send(Ok(response));
                }
            }
        }
    }
    if !ready {
        let _ = ready_tx.send(Err("Python Worker 意外退出".to_string()));
    }
    if let Ok(mut pending) = inner.pending.lock() {
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err("Python Worker 已退出".to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{http::StatusCode, test, App};

    #[derive(Clone)]
    struct EchoAdapter;

    impl RouteAdapter for EchoAdapter {
        fn dispatch(&self, request: AdapterRequest) -> Result<AdapterResponse, String> {
            Ok(AdapterResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: serde_json::to_vec(&request.headers).unwrap(),
            })
        }
    }

    #[actix_web::test]
    async fn rejects_missing_public_session_token() {
        let state = LocalApiState {
            public_session_token: "public-token".into(),
            worker_session_token: "worker-token".into(),
            adapter: Arc::new(EchoAdapter),
        };
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .app_data(web::PayloadConfig::new(MAX_REQUEST_BYTES))
                .default_service(web::route().to(dispatch)),
        )
        .await;
        let response = test::call_service(
            &app,
            test::TestRequest::get().uri("/api/courses").to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn hides_public_session_and_injects_private_worker_session() {
        let state = LocalApiState {
            public_session_token: "public-token".into(),
            worker_session_token: "worker-token".into(),
            adapter: Arc::new(EchoAdapter),
        };
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .app_data(web::PayloadConfig::new(MAX_REQUEST_BYTES))
                .default_service(web::route().to(dispatch)),
        )
        .await;
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/courses")
                .insert_header((SESSION_HEADER, "public-token"))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = test::read_body(response).await;
        let headers: Vec<(String, String)> = serde_json::from_slice(&body).unwrap();
        assert!(headers
            .iter()
            .any(|(name, value)| name == SESSION_HEADER && value == "worker-token"));
        assert!(!headers.iter().any(|(_, value)| value == "public-token"));
    }
}
