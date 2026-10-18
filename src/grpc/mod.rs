//! Dynamic gRPC client: messages described at runtime (`.proto` files compiled by `protox`, or server
//! reflection), JSON ↔ protobuf with `prost-reflect`, calls with `tonic` over HTTP/2 (TLS for `https://`,
//! h2c for `http://`). The API is blocking: each call runs on a private current-thread tokio runtime.

mod json;
mod reflection;
#[cfg(test)]
mod tests;

use std::error::Error as StdError;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, MethodDescriptor};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::metadata::MetadataMap;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Code, Status};

/// Largest message accepted from a server.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// A set of message / service descriptions.
#[derive(Clone, Debug)]
pub struct Protos {
    pool: DescriptorPool,
}

#[derive(Clone, Debug)]
pub struct ServiceInfo {
    /// Full name: `pkg.Svc`.
    pub name: String,
    pub methods: Vec<MethodInfo>,
}

#[derive(Clone, Debug)]
pub struct MethodInfo {
    pub name: String,
    /// `/pkg.Svc/Method`.
    pub full_path: String,
    /// Full name of the request message.
    pub input: String,
    /// Full name of the response message.
    pub output: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
}

#[derive(Clone, Debug)]
pub struct CallRequest {
    /// `http(s)://host:port`.
    pub url: String,
    /// `/pkg.Svc/Method` (`pkg.Svc/Method` and `pkg.Svc.Method` work too).
    pub method_path: String,
    /// JSON messages: one for unary / server streaming, any number for client / bidi streaming (sent, then half-close).
    pub messages: Vec<String>,
    /// Request metadata. `-bin` keys take base64 values.
    pub metadata: Vec<(String, String)>,
    /// Deadline of the whole call (zero = none).
    pub timeout: Duration,
}

#[derive(Clone, Debug, Default)]
pub struct CallResult {
    pub status_code: i32,
    /// `OK`, `NOT_FOUND`…
    pub status_name: String,
    pub status_message: String,
    /// `grpc-status-details-bin` decoded (`google.rpc.Status` details as JSON when their type is known).
    pub status_details: String,
    pub headers: Vec<(String, String)>,
    pub trailers: Vec<(String, String)>,
    /// Responses as pretty JSON.
    pub messages: Vec<String>,
    pub elapsed: Duration,
}

impl MethodInfo {
    fn of(m: &MethodDescriptor) -> MethodInfo {
        MethodInfo {
            name: m.name().to_string(),
            full_path: format!("/{}/{}", m.parent_service().full_name(), m.name()),
            input: m.input().full_name().to_string(),
            output: m.output().full_name().to_string(),
            client_streaming: m.is_client_streaming(),
            server_streaming: m.is_server_streaming(),
        }
    }

    /// `unary`, `server streaming`, `client streaming` or `bidi streaming`.
    pub fn kind(&self) -> &'static str {
        match (self.client_streaming, self.server_streaming) {
            (false, false) => "unary",
            (false, true) => "server streaming",
            (true, false) => "client streaming",
            (true, true) => "bidi streaming",
        }
    }
}

impl Protos {
    /// Compiles `.proto` files. Each file's folder is an include folder too (after `include_dirs`);
    /// the well-known types (`google/protobuf/*.proto`) are built in.
    pub fn from_files(files: &[PathBuf], include_dirs: &[PathBuf]) -> Result<Protos, String> {
        if files.is_empty() {
            return Err("no .proto file given".into());
        }
        let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let mut includes: Vec<PathBuf> = include_dirs.iter().map(|d| abs(d)).collect();
        let files: Vec<PathBuf> = files.iter().map(|f| abs(f)).collect();
        for f in &files {
            if !f.is_file() {
                return Err(format!("{}: no such file", f.display()));
            }
            if !includes.iter().any(|d| f.starts_with(d)) {
                if let Some(parent) = f.parent() {
                    includes.push(parent.to_path_buf());
                }
            }
        }
        let mut compiler = protox::Compiler::new(&includes).map_err(|e| protox_error(&e))?;
        compiler.include_imports(true).include_source_info(false);
        compiler.open_files(&files).map_err(|e| protox_error(&e))?;
        Ok(Protos { pool: compiler.descriptor_pool() })
    }

    /// Asks a server for its services with gRPC server reflection (`grpc.reflection.v1`, else `v1alpha`).
    pub fn from_reflection(url: &str, metadata: &[(String, String)], timeout: Duration) -> Result<Protos, String> {
        let rt = runtime()?;
        let pool = rt.block_on(async {
            let fut = async {
                let channel = connect(url, timeout).await?;
                reflection::fetch(channel, metadata).await
            };
            if timeout.is_zero() {
                fut.await
            } else {
                tokio::time::timeout(timeout, fut).await.unwrap_or_else(|_| Err(format!("reflection: no answer within {timeout:?}")))
            }
        })?;
        Ok(Protos { pool })
    }

    /// From an encoded `FileDescriptorSet` (`protoc --descriptor_set_out`, or [`Protos::encode`]).
    pub fn from_descriptor_set(bytes: &[u8]) -> Result<Protos, String> {
        DescriptorPool::decode(bytes).map(|pool| Protos { pool }).map_err(|e| format!("invalid descriptor set: {e}"))
    }

    /// Everything as an encoded `FileDescriptorSet` (to keep what reflection returned).
    pub fn encode(&self) -> Vec<u8> {
        prost_types::FileDescriptorSet { file: self.pool.file_descriptor_protos().cloned().collect() }.encode_to_vec()
    }

    /// The services, sorted by name.
    pub fn services(&self) -> Vec<ServiceInfo> {
        let mut out: Vec<ServiceInfo> =
            self.pool.services().map(|s| ServiceInfo { name: s.full_name().to_string(), methods: s.methods().map(|m| MethodInfo::of(&m)).collect() }).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The method at `/pkg.Svc/Method` (or `pkg.Svc/Method`, `pkg.Svc.Method`).
    pub fn method(&self, path: &str) -> Option<MethodInfo> {
        self.find_method(path).map(|m| MethodInfo::of(&m))
    }

    /// Names of the messages, sorted.
    pub fn messages(&self) -> Vec<String> {
        let mut v: Vec<String> = self.pool.all_messages().filter(|m| !m.is_map_entry()).map(|m| m.full_name().to_string()).collect();
        v.sort();
        v
    }

    /// A JSON skeleton of a message with every field at its default (nested messages expanded a few levels,
    /// repeated fields with one element, enums as their first value name), for the editor.
    pub fn example_json(&self, message_full_name: &str) -> Option<String> {
        let desc = self.pool.get_message_by_name(message_full_name.trim_start_matches('.'))?;
        serde_json::to_string_pretty(&json::example(&desc)).ok()
    }

    /// A JSON message checked against its description (errors name the field), then re-encoded as pretty JSON.
    pub fn normalize_json(&self, message_full_name: &str, text: &str) -> Result<String, String> {
        let desc = self.pool.get_message_by_name(message_full_name).ok_or_else(|| format!("unknown message {message_full_name}"))?;
        Ok(json::to_json(&json::parse(&desc, text)?))
    }

    fn find_method(&self, path: &str) -> Option<MethodDescriptor> {
        let p = path.trim().trim_start_matches('/');
        let (svc, method) = p.rsplit_once('/').or_else(|| p.rsplit_once('.'))?;
        self.pool.get_service_by_name(svc)?.methods().find(|m| m.name() == method)
    }
}

/// Sends a call and waits for its end. `on_message` gets each response (pretty JSON) as it arrives.
/// `Err` only for local / transport errors; a non-OK gRPC status is `Ok` with `status_code != 0`
/// (a deadline reached gives `DEADLINE_EXCEEDED`, with the responses received before it).
pub fn call(protos: &Protos, req: &CallRequest, on_message: &mut dyn FnMut(&str)) -> Result<CallResult, String> {
    let method = protos.find_method(&req.method_path).ok_or_else(|| format!("unknown method {}", req.method_path))?;
    let texts: Vec<&str> = if req.messages.is_empty() { vec!["{}"] } else { req.messages.iter().map(|s| s.as_str()).collect() };
    if !method.is_client_streaming() && texts.len() > 1 {
        return Err(format!("{} takes one message ({} given)", method.name(), texts.len()));
    }
    let input = method.input();
    let mut msgs = Vec::with_capacity(texts.len());
    for (i, t) in texts.iter().enumerate() {
        let m = json::parse(&input, t).map_err(|e| if texts.len() > 1 { format!("message {}: {e}", i + 1) } else { e })?;
        msgs.push(m);
    }
    let path = format!("/{}/{}", method.parent_service().full_name(), method.name());
    let path = http::uri::PathAndQuery::try_from(path.as_str()).map_err(|e| format!("invalid method path {path}: {e}"))?;
    let headers = metadata_map(&req.metadata)?;
    let rt = runtime()?;
    let start = Instant::now();
    let mut out = Outcome::default();
    rt.block_on(async {
        let channel = connect(&req.url, req.timeout).await?;
        let mut request = tonic::Request::new(tokio_stream::iter(msgs));
        *request.metadata_mut() = headers;
        if !req.timeout.is_zero() {
            request.set_timeout(req.timeout);
        }
        let codec = DynCodec::<DynamicMessage, DynamicMessage>::new(method.output());
        let fut = run_call(channel, request, path, codec, &mut out, on_message);
        if req.timeout.is_zero() {
            return fut.await;
        }
        let ended = tokio::time::timeout(req.timeout, fut).await;
        match ended {
            Ok(r) => r,
            Err(_) => {
                out.set_status(&Status::deadline_exceeded(format!("no end of call within {:?}", req.timeout)));
                Ok(())
            }
        }
    })?;
    let mut result = out.result;
    result.elapsed = start.elapsed();
    if !out.details.is_empty() {
        result.status_details = details_text(protos, &out.details);
    }
    Ok(result)
}

async fn run_call(
    channel: Channel,
    request: tonic::Request<tokio_stream::Iter<std::vec::IntoIter<DynamicMessage>>>,
    path: http::uri::PathAndQuery,
    codec: DynCodec<DynamicMessage, DynamicMessage>,
    out: &mut Outcome,
    on_message: &mut dyn FnMut(&str),
) -> Result<(), String> {
    let mut grpc = tonic::client::Grpc::new(channel).max_decoding_message_size(MAX_MESSAGE).max_encoding_message_size(MAX_MESSAGE);
    grpc.ready().await.map_err(|e| error_chain(&e))?;
    let response = match grpc.streaming(request, path, codec).await {
        Ok(r) => r,
        Err(status) if is_local(&status) => return Err(error_chain(&status)),
        Err(status) => {
            // Trailers-only response: its headers are the trailers.
            out.result.trailers = status_trailers(&status);
            out.set_status(&status);
            return Ok(());
        }
    };
    out.result.headers = metadata_pairs(response.metadata());
    let mut stream = response.into_inner();
    loop {
        match stream.message().await {
            Ok(Some(m)) => {
                let text = json::to_json(&m);
                on_message(&text);
                out.result.messages.push(text);
            }
            Ok(None) => {
                match stream.trailers().await {
                    Ok(t) => {
                        out.result.trailers = t.map(|t| metadata_pairs(&t)).unwrap_or_default();
                        out.set_status(&Status::ok(""));
                    }
                    Err(status) => {
                        out.result.trailers = status_trailers(&status);
                        out.set_status(&status);
                    }
                }
                return Ok(());
            }
            Err(status) => {
                // A transport error after some responses keeps them: the status says what happened.
                out.set_status(&status);
                if is_local(&status) {
                    out.result.status_message = error_chain(&status);
                } else {
                    out.result.trailers = status_trailers(&status);
                }
                return Ok(());
            }
        }
    }
}

/// A status's metadata, `grpc-status` / `grpc-message` included (tonic takes them out).
fn status_trailers(status: &Status) -> Vec<(String, String)> {
    let mut t = metadata_pairs(status.metadata());
    if !t.iter().any(|(k, _)| k == "grpc-status") {
        t.push(("grpc-status".into(), (status.code() as i32).to_string()));
        if !status.message().is_empty() {
            t.push(("grpc-message".into(), status.message().to_string()));
        }
    }
    t
}

/// A call's result while it runs, with the raw status details.
#[derive(Default)]
struct Outcome {
    result: CallResult,
    details: Vec<u8>,
}

impl Outcome {
    fn set_status(&mut self, status: &Status) {
        self.result.status_code = status.code() as i32;
        self.result.status_name = code_name(status.code() as i32).to_string();
        self.result.status_message = status.message().to_string();
        self.details = status.details().to_vec();
    }
}

/// `google.rpc.Status` details as text: one line per detail, JSON when the pool knows its type.
fn details_text(protos: &Protos, bytes: &[u8]) -> String {
    let Ok(st) = RpcStatus::decode(bytes) else { return format!("{} bytes (not a google.rpc.Status)", bytes.len()) };
    let mut lines = Vec::new();
    for any in &st.details {
        let name = any.type_url.rsplit('/').next().unwrap_or(&any.type_url);
        let decoded = protos
            .pool
            .get_message_by_name(name)
            .and_then(|d| DynamicMessage::decode(d, any.value.as_slice()).ok())
            .and_then(|m| m.serialize_with_options(serde_json::value::Serializer, &json::serialize_options()).ok());
        lines.push(match decoded {
            Some(v) => format!("{name}: {v}"),
            None => format!("{name}: {} bytes", any.value.len()),
        });
    }
    if lines.is_empty() {
        format!("google.rpc.Status {{ code: {}, message: {:?} }}", st.code, st.message)
    } else {
        lines.join("\n")
    }
}

/// `google.rpc.Status`, what `grpc-status-details-bin` holds.
#[derive(Clone, PartialEq, prost::Message)]
struct RpcStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
    #[prost(message, repeated, tag = "3")]
    details: Vec<prost_types::Any>,
}

/// The canonical name of a gRPC status code.
pub fn code_name(code: i32) -> &'static str {
    match code {
        0 => "OK",
        1 => "CANCELLED",
        2 => "UNKNOWN",
        3 => "INVALID_ARGUMENT",
        4 => "DEADLINE_EXCEEDED",
        5 => "NOT_FOUND",
        6 => "ALREADY_EXISTS",
        7 => "PERMISSION_DENIED",
        8 => "RESOURCE_EXHAUSTED",
        9 => "FAILED_PRECONDITION",
        10 => "ABORTED",
        11 => "OUT_OF_RANGE",
        12 => "UNIMPLEMENTED",
        13 => "INTERNAL",
        14 => "UNAVAILABLE",
        15 => "DATA_LOSS",
        16 => "UNAUTHENTICATED",
        _ => "UNKNOWN_CODE",
    }
}

/// A status made by the client itself (connection lost, protocol error) rather than sent by the server.
fn is_local(status: &Status) -> bool {
    status.source().is_some() && status.metadata().is_empty()
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("tokio runtime: {e}"))
}

/// Opens the HTTP/2 connection: TLS (system + webpki roots) for `https://`, h2c for `http://`.
async fn connect(url: &str, timeout: Duration) -> Result<Channel, String> {
    let url = url.trim().trim_end_matches('/');
    let tls = if url.starts_with("https://") {
        true
    } else if url.starts_with("http://") {
        false
    } else {
        return Err(format!("{url}: the address must start with http:// or https://"));
    };
    let mut ep = Endpoint::from_shared(url.to_string()).map_err(|e| format!("{url}: {}", error_chain(&e)))?.user_agent("apifa").map_err(|e| error_chain(&e))?;
    if !timeout.is_zero() {
        ep = ep.connect_timeout(timeout);
    }
    if tls {
        ep = ep.tls_config(ClientTlsConfig::new().with_enabled_roots()).map_err(|e| format!("TLS: {}", error_chain(&e)))?;
    }
    ep.connect().await.map_err(|e| format!("can't connect to {url}: {}", error_chain(&e)))
}

/// An error and its causes: `a: b: c` (without repeats).
fn error_chain(e: &dyn StdError) -> String {
    let mut parts: Vec<String> = vec![e.to_string()];
    let mut cur = e.source();
    while let Some(c) = cur {
        let s = c.to_string();
        if !parts.iter().any(|p| p.contains(&s)) {
            parts.push(s);
        }
        cur = c.source();
    }
    parts.join(": ")
}

fn metadata_map(pairs: &[(String, String)]) -> Result<MetadataMap, String> {
    let mut map = http::HeaderMap::new();
    for (k, v) in pairs {
        let k = k.trim().to_ascii_lowercase();
        if k.is_empty() {
            continue;
        }
        let name = http::HeaderName::from_bytes(k.as_bytes()).map_err(|_| format!("invalid metadata key {k:?}"))?;
        let value = http::HeaderValue::from_str(v.trim()).map_err(|_| format!("invalid value for metadata {k:?} (text only; base64 for -bin keys)"))?;
        map.append(name, value);
    }
    Ok(MetadataMap::from_headers(map))
}

fn metadata_pairs(m: &MetadataMap) -> Vec<(String, String)> {
    m.clone().into_headers().iter().map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect()
}

/// `file:line:col: message` from a protox error.
fn protox_error(e: &protox::Error) -> String {
    use miette::Diagnostic;
    let msg = e.to_string();
    let at = e.labels().and_then(|mut l| l.next()).and_then(|label| {
        let src = e.source_code()?;
        let c = src.read_span(label.inner(), 0, 0).ok()?;
        Some(format!("{}:{}:{}", c.name().map(str::to_string).or_else(|| e.file().map(str::to_string)).unwrap_or_default(), c.line() + 1, c.column() + 1))
    });
    match (at, e.file()) {
        (Some(at), _) => format!("{at}: {msg}"),
        (None, Some(f)) if !msg.contains(f) => format!("{f}: {msg}"),
        _ => msg,
    }
}

/// Messages the codec knows how to decode.
trait Decodable: Sized + Send + Sync + 'static {
    type Ctx: Clone + Send + 'static;
    fn decode_from(ctx: &Self::Ctx, buf: &mut DecodeBuf<'_>) -> Result<Self, prost::DecodeError>;
}

impl Decodable for DynamicMessage {
    type Ctx = MessageDescriptor;
    fn decode_from(ctx: &MessageDescriptor, buf: &mut DecodeBuf<'_>) -> Result<Self, prost::DecodeError> {
        DynamicMessage::decode(ctx.clone(), buf)
    }
}

/// A tonic codec for any prost message going out and a [`Decodable`] coming back.
struct DynCodec<E, D: Decodable> {
    ctx: D::Ctx,
    _e: std::marker::PhantomData<fn(E)>,
}

impl<E, D: Decodable> DynCodec<E, D> {
    fn new(ctx: D::Ctx) -> Self {
        DynCodec { ctx, _e: std::marker::PhantomData }
    }
}

struct DynEncoder<E>(std::marker::PhantomData<fn(E)>);
struct DynDecoder<D: Decodable>(D::Ctx);

impl<E: prost::Message + Send + Sync + 'static, D: Decodable> Codec for DynCodec<E, D> {
    type Encode = E;
    type Decode = D;
    type Encoder = DynEncoder<E>;
    type Decoder = DynDecoder<D>;
    fn encoder(&mut self) -> Self::Encoder {
        DynEncoder(std::marker::PhantomData)
    }
    fn decoder(&mut self) -> Self::Decoder {
        DynDecoder(self.ctx.clone())
    }
}

impl<E: prost::Message> Encoder for DynEncoder<E> {
    type Item = E;
    type Error = Status;
    fn encode(&mut self, item: E, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst).map_err(|e| Status::internal(format!("encoding the request: {e}")))
    }
}

impl<D: Decodable> Decoder for DynDecoder<D> {
    type Item = D;
    type Error = Status;
    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<D>, Status> {
        D::decode_from(&self.0, src).map(Some).map_err(|e| Status::new(Code::Internal, format!("decoding the response: {e}")))
    }
}
