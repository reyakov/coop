use std::collections::HashMap;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use atomic_destructor::{AtomicDestroyer, AtomicDestructor};
use event_listener::Event as ShutdownEvent;
use nostr::prelude::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};
use smol::channel;
use smol::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use smol::lock::Mutex;
use smol::net::{TcpListener, TcpStream};
use uuid::Uuid;

mod error;
pub mod prelude;

pub use self::error::Error;

const DEFAULT_HTML: &str = include_str!("../index.html");
const JS: &str = include_str!("../proxy.js");

/// Cap on the request line and headers so a peer cannot force unbounded reads
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Cap on the request body so a huge Content-Length cannot force a huge allocation
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Maximum time a connection may stay open before it is dropped
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

type PendingResponseMap = HashMap<Uuid, channel::Sender<Result<Value, String>>>;

#[derive(Debug, Deserialize)]
struct Message {
    id: Uuid,
    error: Option<String>,
    result: Option<Value>,
}

impl Message {
    fn into_result(self) -> Result<Value, String> {
        if let Some(error) = self.error {
            Err(error)
        } else {
            Ok(self.result.unwrap_or(Value::Null))
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RequestMethod {
    GetPublicKey,
    SignEvent,
    Nip44Encrypt,
    Nip44Decrypt,
}

impl RequestMethod {
    fn as_str(&self) -> &str {
        match self {
            Self::GetPublicKey => "get_public_key",
            Self::SignEvent => "sign_event",
            Self::Nip44Encrypt => "nip44_encrypt",
            Self::Nip44Decrypt => "nip44_decrypt",
        }
    }
}

impl Serialize for RequestMethod {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
struct RequestData {
    id: Uuid,
    method: RequestMethod,
    params: Value,
}

impl RequestData {
    #[inline]
    fn new(method: RequestMethod, params: Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            method,
            params,
        }
    }
}

#[derive(Serialize)]
struct Requests<'a> {
    requests: &'a [RequestData],
}

impl<'a> Requests<'a> {
    #[inline]
    fn new(requests: &'a [RequestData]) -> Self {
        Self { requests }
    }

    #[inline]
    fn len(&self) -> usize {
        self.requests.len()
    }
}

/// Params for NIP-04 and NIP-44 encryption/decryption
#[derive(Serialize)]
struct CryptoParams<'a> {
    public_key: &'a PublicKey,
    content: &'a str,
}

impl<'a> CryptoParams<'a> {
    #[inline]
    fn new(public_key: &'a PublicKey, content: &'a str) -> Self {
        Self {
            public_key,
            content,
        }
    }
}

#[derive(Debug)]
struct ProxyState {
    /// Requests waiting to be picked up by browser
    pub outgoing_requests: Mutex<Vec<RequestData>>,
    /// Map of request ID to response sender
    pub pending_responses: Mutex<PendingResponseMap>,
    /// Last time the client asked for the pending requests
    pub last_pending_request: Arc<AtomicU64>,
}

/// Configuration options for [`BrowserSignerProxy`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserSignerProxyOptions {
    /// Request timeout for the signer extension. Default is 30 seconds.
    pub timeout: Duration,
    /// Proxy server IP address and port. Default is `127.0.0.1:7400`.
    pub addr: SocketAddr,
    /// Custom HTML page.
    // NOTE: not `Option` to move it between threads without reference counter
    pub custom_html: &'static str,
}

#[derive(Debug, Clone)]
struct InnerBrowserSignerProxy {
    /// Configuration options for the proxy
    options: BrowserSignerProxyOptions,
    /// Internal state of the proxy including request queues
    state: Arc<ProxyState>,
    /// Notification trigger for graceful shutdown
    shutdown: Arc<ShutdownEvent>,
    /// Flag to indicate if the server is shutdown
    is_shutdown: Arc<AtomicBool>,
    /// Flag indicating if the server is started
    is_started: Arc<AtomicBool>,
}

impl AtomicDestroyer for InnerBrowserSignerProxy {
    fn on_destroy(&self) {
        self.shutdown();
    }
}

impl InnerBrowserSignerProxy {
    #[inline]
    fn is_shutdown(&self) -> bool {
        self.is_shutdown.load(Ordering::SeqCst)
    }

    fn shutdown(&self) {
        // Mark the server as shutdown
        self.is_shutdown.store(true, Ordering::SeqCst);

        // Notify all waiters that the proxy is shutting down
        self.shutdown.notify(usize::MAX);
    }
}

/// Nostr Browser Signer Proxy
///
/// Proxy to use Nostr Browser signer (NIP-07) in native applications.
#[derive(Debug, Clone)]
pub struct BrowserSignerProxy {
    inner: AtomicDestructor<InnerBrowserSignerProxy>,
}

impl Default for BrowserSignerProxyOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            // 7 for NIP-07 and 400 because the NIP title is 40 bytes :)
            addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 7400)),
            custom_html: "",
        }
    }
}

impl BrowserSignerProxy {
    /// Construct a new browser signer proxy
    pub fn new(options: BrowserSignerProxyOptions) -> Self {
        let state = ProxyState {
            outgoing_requests: Mutex::new(Vec::new()),
            pending_responses: Mutex::new(HashMap::new()),
            last_pending_request: Arc::new(AtomicU64::new(0)),
        };

        Self {
            inner: AtomicDestructor::new(InnerBrowserSignerProxy {
                options,
                state: Arc::new(state),
                shutdown: Arc::new(ShutdownEvent::new()),
                is_shutdown: Arc::new(AtomicBool::new(false)),
                is_started: Arc::new(AtomicBool::new(false)),
            }),
        }
    }

    /// Checks if there is an open browser tab ready to respond
    /// to requests by verifying the time since the last pending request.
    #[inline]
    pub fn is_session_active(&self) -> bool {
        current_time() - self.inner.state.last_pending_request.load(Ordering::SeqCst) < 2
    }

    /// Get the signer proxy webpage URL
    #[inline]
    pub fn url(&self) -> String {
        format!("http://{}", self.inner.options.addr)
    }

    /// Start the proxy server.
    ///
    /// If this is not called explicitly, the server will be automatically
    /// started on the first interaction with the signer.
    pub async fn start(&self) -> Result<(), Error> {
        // Ensure is not shutdown
        if self.inner.is_shutdown() {
            return Err(Error::shutdown());
        }

        // Mark the proxy as started and check if was already started
        let is_started: bool = self.inner.is_started.swap(true, Ordering::SeqCst);

        // Immediately return if already started
        if is_started {
            return Ok(());
        }

        let listener: TcpListener = match TcpListener::bind(self.inner.options.addr).await {
            Ok(listener) => listener,
            Err(e) => {
                // Undo the started flag if binding fails
                self.inner.is_started.store(false, Ordering::SeqCst);
                return Err(Error::from(e));
            }
        };

        let addr: SocketAddr = self.inner.options.addr;
        let state: Arc<ProxyState> = self.inner.state.clone();
        let custom_html: &'static str = self.inner.options.custom_html;
        let shutdown: Arc<ShutdownEvent> = self.inner.shutdown.clone();
        // Regenerated on every startup, never part of the opened URL
        let session_token: String = Uuid::new_v4().simple().to_string();
        // Only the exact host:port the server binds may appear in the Host header
        let expected_host: String = addr.to_string();

        smol::spawn(async move {
            tracing::info!("Starting proxy server on {addr}");

            loop {
                // Race between accepting a new connection and shutdown signal
                let shutdown_listener = shutdown.listen();

                enum AcceptEvent {
                    Connection(Result<(TcpStream, SocketAddr), std::io::Error>),
                    Shutdown,
                }

                let event = smol::future::or(
                    async { AcceptEvent::Connection(listener.accept().await) },
                    async {
                        shutdown_listener.await;
                        AcceptEvent::Shutdown
                    },
                )
                .await;

                match event {
                    AcceptEvent::Connection(Ok((stream, _))) => {
                        let state: Arc<ProxyState> = state.clone();
                        let shutdown: Arc<ShutdownEvent> = shutdown.clone();
                        let expected_host: String = expected_host.clone();
                        let session_token: String = session_token.clone();

                        smol::spawn(async move {
                            let shutdown_listener = shutdown.listen();

                            smol::future::or(
                                async {
                                    smol::future::or(
                                        async {
                                            handle_connection(
                                                stream,
                                                state,
                                                custom_html,
                                                expected_host,
                                                session_token,
                                            )
                                            .await;
                                        },
                                        async {
                                            smol::Timer::after(CONNECTION_TIMEOUT).await;
                                        },
                                    )
                                    .await;
                                },
                                async {
                                    shutdown_listener.await;
                                    tracing::debug!(
                                        "Closing connection, proxy server is shutting down."
                                    );
                                },
                            )
                            .await;
                        })
                        .detach();
                    }
                    AcceptEvent::Connection(Err(e)) => {
                        tracing::error!("Failed to accept connection: {e}");
                    }
                    AcceptEvent::Shutdown => break,
                }
            }

            tracing::info!("Proxy server shut down.");
        })
        .detach();

        Ok(())
    }

    #[inline]
    async fn store_pending_response(&self, id: Uuid, tx: channel::Sender<Result<Value, String>>) {
        let mut pending_responses = self.inner.state.pending_responses.lock().await;
        pending_responses.insert(id, tx);
    }

    #[inline]
    async fn store_outgoing_request(&self, request: RequestData) {
        let mut outgoing_requests = self.inner.state.outgoing_requests.lock().await;
        outgoing_requests.push(request);
    }

    /// Drop a request from both queues so timed-out request data is not retained.
    #[inline]
    async fn cancel_request(&self, id: Uuid) {
        self.inner.state.pending_responses.lock().await.remove(&id);
        self.inner
            .state
            .outgoing_requests
            .lock()
            .await
            .retain(|request| request.id != id);
    }

    async fn request<T>(&self, method: RequestMethod, params: Value) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        // Start the proxy if not already started
        self.start().await?;

        // Construct the request
        let request: RequestData = RequestData::new(method, params);
        let request_id = request.id;

        // Create a bounded channel of size 1 as a oneshot replacement
        let (tx, rx) = channel::bounded::<Result<Value, String>>(1);

        // Store the response sender
        self.store_pending_response(request.id, tx).await;

        // Add to outgoing requests queue
        self.store_outgoing_request(request).await;

        // Wait for response with timeout
        let response = race_timeout(self.inner.options.timeout, rx.recv()).await;

        match response {
            Ok(Ok(res)) => Ok(serde_json::from_value(res)?),
            Ok(Err(error)) => Err(Error::generic(error)),
            Err(TimeoutError) => {
                self.cancel_request(request_id).await;
                Err(Error::timeout())
            }
        }
    }

    #[inline]
    async fn _get_public_key(&self) -> Result<PublicKey, Error> {
        self.request(RequestMethod::GetPublicKey, json!({})).await
    }

    #[inline]
    async fn _sign_event(&self, event: UnsignedEvent) -> Result<Event, Error> {
        let expected_id: EventId = event.compute_id();
        let event: Event = self
            .request(RequestMethod::SignEvent, serde_json::to_value(event)?)
            .await?;
        event.verify()?;

        if event.id != expected_id {
            return Err(Error::generic(format!(
                "signed event doesn't match the requested event: expected ID {expected_id}, got {}",
                event.id
            )));
        }

        Ok(event)
    }

    #[inline]
    async fn _nip44_encrypt(&self, public_key: &PublicKey, content: &str) -> Result<String, Error> {
        let params = CryptoParams::new(public_key, content);
        self.request(RequestMethod::Nip44Encrypt, serde_json::to_value(params)?)
            .await
    }

    #[inline]
    async fn _nip44_decrypt(&self, public_key: &PublicKey, content: &str) -> Result<String, Error> {
        let params = CryptoParams::new(public_key, content);
        self.request(RequestMethod::Nip44Decrypt, serde_json::to_value(params)?)
            .await
    }
}

impl AsyncGetPublicKey for BrowserSignerProxy {
    type Error = Error;

    #[inline]
    fn get_public_key_async(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PublicKey, Self::Error>> + Send + '_>> {
        Box::pin(async move { self._get_public_key().await })
    }
}

impl AsyncSignEvent for BrowserSignerProxy {
    type Error = Error;

    #[inline]
    fn sign_event_async(
        &self,
        unsigned: UnsignedEvent,
    ) -> Pin<Box<dyn Future<Output = Result<Event, Self::Error>> + Send + '_>> {
        Box::pin(async move { self._sign_event(unsigned).await })
    }
}

impl AsyncNip44 for BrowserSignerProxy {
    type Error = Error;

    fn nip44_encrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, Self::Error>> + Send + 'a>> {
        Box::pin(async move { self._nip44_encrypt(public_key, content).await })
    }

    fn nip44_decrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        payload: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, Self::Error>> + Send + 'a>> {
        Box::pin(async move { self._nip44_decrypt(public_key, payload).await })
    }
}

/// Outcome of reading a line with [`read_line_capped`].
enum HeaderLine {
    /// A line, including the trailing newline when present
    Line(Vec<u8>),
    /// Clean EOF with no data
    Eof,
    /// The line exceeded the allowed length
    TooLong,
}

/// Read one `\n`-terminated line with a hard cap so a peer cannot force unbounded buffering.
async fn read_line_capped(
    reader: &mut BufReader<TcpStream>,
    limit: usize,
) -> Result<HeaderLine, std::io::Error> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let (data, line_complete) = {
            let chunk = reader.fill_buf().await?;
            if chunk.is_empty() {
                break;
            }
            match chunk.iter().position(|byte| *byte == b'\n') {
                Some(index) => (chunk[..=index].to_vec(), true),
                None => (chunk.to_vec(), false),
            }
        };

        if line.len() + data.len() > limit {
            return Ok(HeaderLine::TooLong);
        }

        let taken = data.len();
        line.extend_from_slice(&data);
        reader.consume(taken);

        if line_complete {
            return Ok(HeaderLine::Line(line));
        }
    }

    Ok(if line.is_empty() {
        HeaderLine::Eof
    } else {
        HeaderLine::Line(line)
    })
}

/// Handle a single HTTP connection.
async fn handle_connection(
    stream: TcpStream,
    state: Arc<ProxyState>,
    custom_html: &'static str,
    expected_host: String,
    session_token: String,
) {
    let mut reader = BufReader::new(stream);
    let mut header_bytes: usize = 0;

    // Read the request line
    let request_line = match read_line_capped(&mut reader, MAX_HEADER_BYTES).await {
        Ok(HeaderLine::Line(line)) => line,
        Ok(HeaderLine::Eof) => Vec::new(),
        Ok(HeaderLine::TooLong) => {
            send_response(&mut reader, 400, "Bad Request", "", "").await;
            return;
        }
        Err(_) => return,
    };
    header_bytes += request_line.len();
    let request_line = match String::from_utf8(request_line) {
        Ok(line) => line.trim_end().to_string(),
        Err(_) => {
            send_response(&mut reader, 400, "Bad Request", "", "").await;
            return;
        }
    };

    // Parse method, path, and HTTP version from request line
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        send_response(&mut reader, 400, "Bad Request", "", "").await;
        return;
    }
    let method = parts[0].to_uppercase();
    let path = parts[1].to_string();

    // Read headers until empty line
    let mut headers = Vec::new();
    let mut content_length: usize = 0;
    let mut host: Option<String> = None;

    loop {
        let line = match read_line_capped(&mut reader, MAX_HEADER_BYTES).await {
            Ok(HeaderLine::Line(line)) => line,
            Ok(HeaderLine::Eof) => break,
            Ok(HeaderLine::TooLong) => {
                send_response(&mut reader, 400, "Bad Request", "", "").await;
                return;
            }
            Err(_) => return,
        };

        header_bytes += line.len();
        if header_bytes > MAX_HEADER_BYTES {
            send_response(&mut reader, 400, "Bad Request", "", "").await;
            return;
        }

        let line = match String::from_utf8(line) {
            Ok(line) => line.trim_end().to_string(),
            Err(_) => {
                send_response(&mut reader, 400, "Bad Request", "", "").await;
                return;
            }
        };
        if line.is_empty() {
            break;
        }

        if let Some(value) = line.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("host:") {
            host = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Host:") {
            host = Some(value.trim().to_string());
        }

        headers.push(line);
    }

    // Reject oversized bodies before allocating the read buffer
    if content_length > MAX_BODY_BYTES {
        send_response(&mut reader, 413, "Payload Too Large", "", "").await;
        return;
    }

    // DNS rebinding defense: only the exact bound host:port is accepted
    if host.as_deref() != Some(expected_host.as_str()) {
        send_response(&mut reader, 403, "Forbidden", "", "").await;
        return;
    }

    let (route, query) = match path.split_once('?') {
        Some((route, query)) => (route, Some(query)),
        None => (path.as_str(), None),
    };

    if route.starts_with("/api/") && query_param(query, "token") != Some(session_token.as_str()) {
        send_response(&mut reader, 403, "Forbidden", "", "").await;
        return;
    }

    match (method.as_str(), route) {
        // Serve the HTML proxy page
        ("GET", "/") => {
            let html = if custom_html.is_empty() {
                DEFAULT_HTML
            } else {
                custom_html
            };
            let html = inject_session_token(html, &session_token);
            send_response(&mut reader, 200, "OK", "text/html", &html).await;
        }
        // Serve the JS proxy script
        ("GET", "/proxy.js") => {
            send_response(&mut reader, 200, "OK", "application/javascript", JS).await;
        }
        // Browser polls this endpoint to get pending requests
        ("GET", "/api/pending") => {
            state
                .last_pending_request
                .store(current_time(), Ordering::SeqCst);

            let mut outgoing = state.outgoing_requests.lock().await;

            let requests = Requests::new(&outgoing);
            let json = match serde_json::to_string(&requests) {
                Ok(j) => j,
                Err(e) => {
                    tracing::error!("Failed to serialize pending requests: {e}");
                    send_response(&mut reader, 500, "Internal Server Error", "", "").await;
                    return;
                }
            };

            tracing::debug!("Sending {} pending requests to browser", requests.len());

            // Clear the outgoing requests after sending them
            outgoing.clear();

            send_response(&mut reader, 200, "OK", "application/json", &json).await;
        }
        // Receive response from browser extension
        ("POST", "/api/response") => {
            let mut body_bytes = vec![0u8; content_length];
            if content_length > 0 && reader.read_exact(&mut body_bytes).await.is_err() {
                send_response(&mut reader, 400, "Bad Request", "", "").await;
                return;
            }

            let message: Message = match serde_json::from_slice(&body_bytes) {
                Ok(json) => json,
                Err(e) => {
                    tracing::error!("Failed to parse response body: {e}");
                    send_response(&mut reader, 400, "Invalid JSON", "", "").await;
                    return;
                }
            };

            // Never log the response value, it can contain decrypted plaintext
            let outcome = if message.error.is_some() {
                "error"
            } else {
                "ok"
            };

            tracing::debug!(
                "Received response from browser: id={} outcome={outcome}",
                message.id
            );

            let id: Uuid = message.id;
            let mut pending = state.pending_responses.lock().await;

            match pending.remove(&id) {
                Some(sender) => match sender.try_send(message.into_result()) {
                    Ok(()) => tracing::info!("Forwarded response for request {id}"),
                    Err(e) => {
                        tracing::warn!("Failed to forward response for request {id}: {e}")
                    }
                },
                None => tracing::warn!("No pending request found for {id}"),
            }

            send_response(&mut reader, 200, "OK", "text/plain", "OK").await;
        }
        // 404 - not found
        _ => {
            send_response(&mut reader, 404, "Not Found", "", "").await;
        }
    }
}

/// Write an HTTP response to the stream.
async fn send_response(
    stream: &mut (impl AsyncWriteExt + Unpin),
    status: u16,
    status_text: &str,
    content_type: &str,
    body: &str,
) {
    let mut response = format!("HTTP/1.1 {status} {status_text}\r\n");

    if !content_type.is_empty() {
        response.push_str(&format!("Content-Type: {content_type}\r\n"));
    }

    response.push_str(&format!("Content-Length: {}\r\n", body.len()));
    response.push_str("Connection: close\r\n");
    response.push_str("\r\n");
    response.push_str(body);

    write_response(stream, &response).await;
}

async fn write_response(stream: &mut (impl AsyncWriteExt + Unpin), response: &str) {
    if let Err(e) = stream.write_all(response.as_bytes()).await {
        tracing::debug!("Failed to write response to peer: {e}");
    }
    if let Err(e) = stream.flush().await {
        tracing::debug!("Failed to flush response to peer: {e}");
    }
}

/// Get a parameter value from an optional query string.
fn query_param<'a>(query: Option<&'a str>, name: &str) -> Option<&'a str> {
    let query = query?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

fn inject_session_token(html: &str, token: &str) -> String {
    let script = format!("<script>window.proxySessionToken=\"{token}\";</script>");
    match html.rfind("</body>") {
        Some(index) => format!("{}{script}{}", &html[..index], &html[index..]),
        None => format!("{html}{script}"),
    }
}

/// An error indicating that an operation timed out.
#[derive(Debug)]
struct TimeoutError;

/// Races a channel receive against a duration.
async fn race_timeout<T>(
    duration: Duration,
    recv: impl Future<Output = Result<T, channel::RecvError>>,
) -> Result<T, TimeoutError> {
    enum Event<T> {
        Value(T),
        ChannelClosed,
        Timeout,
    }

    let event = smol::future::or(
        async {
            match recv.await {
                Ok(value) => Event::Value(value),
                Err(_) => Event::ChannelClosed,
            }
        },
        async {
            smol::Timer::after(duration).await;
            Event::Timeout
        },
    )
    .await;

    match event {
        Event::Value(value) => Ok(value),
        Event::ChannelClosed | Event::Timeout => Err(TimeoutError),
    }
}

#[inline]
fn current_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
