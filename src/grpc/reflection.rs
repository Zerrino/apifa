//! gRPC server reflection client (`grpc.reflection.v1`, else `v1alpha`): the services, then the files
//! that define them, then every file they import, into a descriptor pool.

use std::collections::{BTreeMap, HashSet};

use prost::Message as _;
use prost_reflect::DescriptorPool;
use prost_types::FileDescriptorProto;
use tonic::codec::DecodeBuf;
use tonic::transport::Channel;
use tonic::Code;

use super::{error_chain, metadata_map, Decodable, DynCodec, MAX_MESSAGE};

const V1: &str = "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo";
const V1ALPHA: &str = "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo";

/// `ServerReflectionRequest` (same wire format in v1 and v1alpha); the oneof as optional fields.
#[derive(Clone, PartialEq, prost::Message)]
struct Request {
    #[prost(string, tag = "1")]
    host: String,
    #[prost(string, optional, tag = "3")]
    file_by_filename: Option<String>,
    #[prost(string, optional, tag = "4")]
    file_containing_symbol: Option<String>,
    #[prost(string, optional, tag = "7")]
    list_services: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Response {
    #[prost(message, optional, tag = "4")]
    file_descriptor_response: Option<FileDescriptorResponse>,
    #[prost(message, optional, tag = "6")]
    list_services_response: Option<ListServiceResponse>,
    #[prost(message, optional, tag = "7")]
    error_response: Option<ErrorResponse>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct FileDescriptorResponse {
    #[prost(bytes = "vec", repeated, tag = "1")]
    file_descriptor_proto: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ListServiceResponse {
    #[prost(message, repeated, tag = "1")]
    service: Vec<ServiceResponse>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ServiceResponse {
    #[prost(string, tag = "1")]
    name: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ErrorResponse {
    #[prost(int32, tag = "1")]
    error_code: i32,
    #[prost(string, tag = "2")]
    error_message: String,
}

impl Decodable for Response {
    type Ctx = ();
    fn decode_from(_: &(), buf: &mut DecodeBuf<'_>) -> Result<Self, prost::DecodeError> {
        Response::decode(buf)
    }
}

/// Every service of the server and the files they need.
pub(super) async fn fetch(channel: Channel, metadata: &[(String, String)]) -> Result<DescriptorPool, String> {
    let list = Request { list_services: Some(String::new()), ..Default::default() };
    let (path, first) = match round(&channel, V1, metadata, vec![list.clone()]).await {
        Ok(r) => (V1, r),
        Err((Code::Unimplemented, _)) => match round(&channel, V1ALPHA, metadata, vec![list]).await {
            Ok(r) => (V1ALPHA, r),
            Err((Code::Unimplemented, _)) => return Err("the server doesn't implement gRPC reflection".into()),
            Err((_, e)) => return Err(format!("reflection: {e}")),
        },
        Err((_, e)) => return Err(format!("reflection: {e}")),
    };
    let mut services = Vec::new();
    for r in first {
        services.extend(check(r).map_err(|e| format!("reflection: list of services: {e}"))?.list_services_response.into_iter().flat_map(|l| l.service).map(|s| s.name));
    }
    if services.is_empty() {
        return Err("reflection: the server lists no service".into());
    }
    let mut files: BTreeMap<String, FileDescriptorProto> = BTreeMap::new();
    let asks: Vec<Request> = services.iter().map(|s| Request { file_containing_symbol: Some(s.clone()), ..Default::default() }).collect();
    for (resp, svc) in round(&channel, path, metadata, asks).await.map_err(|(_, e)| format!("reflection: {e}"))?.into_iter().zip(&services) {
        add_files(&mut files, check(resp).map_err(|e| format!("reflection: service {svc}: {e}"))?)?;
    }
    // Imports the server didn't send yet, asked by name (the well-known types are built in).
    let mut asked: HashSet<String> = HashSet::new();
    loop {
        let missing: Vec<String> =
            files.values().flat_map(|f| f.dependency.iter()).filter(|d| !files.contains_key(*d)).cloned().collect::<HashSet<_>>().into_iter().collect();
        let mut ask = Vec::new();
        for name in missing {
            if let Some(f) = DescriptorPool::global().get_file_by_name(&name) {
                files.insert(name, f.file_descriptor_proto().clone());
            } else if asked.insert(name.clone()) {
                ask.push(name);
            } else {
                return Err(format!("reflection: the server didn't give {name}"));
            }
        }
        if ask.is_empty() {
            break;
        }
        let reqs = ask.iter().map(|n| Request { file_by_filename: Some(n.clone()), ..Default::default() }).collect();
        for (resp, name) in round(&channel, path, metadata, reqs).await.map_err(|(_, e)| format!("reflection: {e}"))?.into_iter().zip(&ask) {
            add_files(&mut files, check(resp).map_err(|e| format!("reflection: file {name}: {e}"))?)?;
        }
    }
    let mut pool = DescriptorPool::new();
    pool.add_file_descriptor_protos(files.into_values()).map_err(|e| format!("reflection: invalid descriptors: {e}"))?;
    Ok(pool)
}

fn check(r: Response) -> Result<Response, String> {
    match &r.error_response {
        Some(e) => Err(format!("{} ({})", e.error_message, super::code_name(e.error_code))),
        None => Ok(r),
    }
}

fn add_files(files: &mut BTreeMap<String, FileDescriptorProto>, r: Response) -> Result<(), String> {
    for bytes in r.file_descriptor_response.map(|f| f.file_descriptor_proto).unwrap_or_default() {
        let f = FileDescriptorProto::decode(bytes.as_slice()).map_err(|e| format!("reflection: invalid file descriptor: {e}"))?;
        files.entry(f.name().to_string()).or_insert(f);
    }
    Ok(())
}

/// One stream: the requests, half-close, their answers.
async fn round(channel: &Channel, path: &str, metadata: &[(String, String)], reqs: Vec<Request>) -> Result<Vec<Response>, (Code, String)> {
    let n = reqs.len();
    let mut grpc = tonic::client::Grpc::new(channel.clone()).max_decoding_message_size(MAX_MESSAGE);
    grpc.ready().await.map_err(|e| (Code::Unavailable, error_chain(&e)))?;
    let mut request = tonic::Request::new(tokio_stream::iter(reqs));
    *request.metadata_mut() = metadata_map(metadata).map_err(|e| (Code::InvalidArgument, e))?;
    let path = http::uri::PathAndQuery::from_static(if path == V1 { V1 } else { V1ALPHA });
    let codec = DynCodec::<Request, Response>::new(());
    let mut stream = grpc.streaming(request, path, codec).await.map_err(|s| (s.code(), status_text(&s)))?.into_inner();
    let mut out = Vec::with_capacity(n);
    while let Some(r) = stream.message().await.map_err(|s| (s.code(), status_text(&s)))? {
        out.push(r);
    }
    if out.len() < n {
        return Err((Code::Internal, format!("{} answers to {n} requests", out.len())));
    }
    Ok(out)
}

fn status_text(s: &tonic::Status) -> String {
    if s.message().is_empty() {
        super::code_name(s.code() as i32).to_string()
    } else {
        format!("{} ({})", s.message(), super::code_name(s.code() as i32))
    }
}
