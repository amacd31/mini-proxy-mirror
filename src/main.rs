
use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use tokio_util::bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, Result, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::{fs::File, net::TcpListener};
use tokio_util::io::{InspectReader, ReaderStream};


static NOTFOUND: &[u8] = b"Not Found";

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {

    let addr: SocketAddr = "0.0.0.0:3030".parse().unwrap();

    let listener = TcpListener::bind(addr).await?;
    println!("Listening on http://{}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service_fn(stream_request_from_mirror_or_cache))
                .await
            {
                println!("Failed to serve connection: {:?}", err);
            }
        });
    }
}

/// HTTP status code 404
fn not_found() -> Response<BoxBody<Bytes, std::io::Error>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(NOTFOUND.into()).map_err(|e| match e {}).boxed())
        .unwrap()
}

fn write_to_cache(cached_file_path: PathBuf) -> Box<dyn Fn(&[u8]) -> () + Send + Sync> {
    Box::new(move |data: &[u8]| {
            std::fs::create_dir_all(cached_file_path.parent().unwrap()).unwrap();
            let mut file = OpenOptions::new()
                .write(true)
                .append(true)
                .create(true)
                .open(cached_file_path.clone())
                .unwrap();
            file.write_all(data).unwrap();

        ()
    })
}

async fn stream_request_from_mirror_or_cache(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<BoxBody<Bytes, std::io::Error>>> {



    let work_dir = "./cache".to_string();
    println!("{:?}", work_dir);
    let uri = req.uri();
    let cache_uri_path: String = (work_dir + uri.path()).to_owned();
    println!("{:?}", cache_uri_path);

    let cached_file_path = Path::new(&cache_uri_path).to_path_buf();
    println!("{:?}", cached_file_path);
    let status: StatusCode;

    if cached_file_path.exists() && !uri.to_string().ends_with("/") {
        println!("Cached file exists: {:?}", cached_file_path);
        let file = File::open(cached_file_path).await;
        if file.is_err() {
            eprintln!("ERROR: Unable to open file.");
            return Ok(not_found());
        }

        let file: File = file.unwrap();

        let reader_stream = ReaderStream::new(file);
        let stream_body = StreamBody::new(reader_stream.map_ok(Frame::data));

        let boxed_body = stream_body.boxed();
        let response = Response::builder()
            .status(StatusCode::OK)
            .body(boxed_body)
            .unwrap();
        Ok(response)
    }
    else {
        println!("{:?}", req);
        let resp = reqwest::get("http://archlinux.mirror.digitalpacific.com.au".to_owned()+&uri.to_string())
        .await.unwrap();
        println!("{resp:#?}");
        status = resp.status().clone();
        if status.is_success() && !uri.to_string().ends_with("/") {
            let stream = resp.bytes_stream().map_err(std::io::Error::other);
            let async_stream = tokio_util::io::StreamReader::new(stream);
            let curried_write_to_cache = write_to_cache(cached_file_path);
            let reader_inspector = InspectReader::new(async_stream, curried_write_to_cache);
            let reader_stream = ReaderStream::new(reader_inspector);
            let stream_body = StreamBody::new(reader_stream.map_ok(Frame::data));
            let boxed_body = stream_body.boxed();

            let response = Response::builder()
                .status(StatusCode::OK)
                .body(boxed_body)
                .unwrap();
            Ok(response)
        }
        else {
            let stream = resp.bytes_stream().map_err(std::io::Error::other);
            let stream_body = StreamBody::new(stream.map_ok(Frame::data));
            let boxed_body = stream_body.boxed();

            println!("{:?}", status);
            let response = Response::builder()
                .status(status)
                .body(boxed_body)
                .unwrap();
            Ok(response)
        
        }

    }

}