use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use futures_util::TryStreamExt;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, Result, StatusCode};
use hyper_util::rt::TokioIo;
use path_clean::PathClean;
use std::iter::repeat_with;
use tokio::{fs::File, net::TcpListener};
use tokio_util::bytes::Bytes;
use tokio_util::io::{InspectReader, ReaderStream};
use tracing::{debug, error, info, Level};
use tracing_subscriber::FmtSubscriber;

static NOTFOUND: &[u8] = b"Not Found";
static STREAM_BUFFER_SIZE: usize = 512usize.pow(2);

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let addr: SocketAddr = "0.0.0.0:3030".parse().unwrap();

    let listener = TcpListener::bind(addr).await?;
    info!("Listening on http://{}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service_fn(stream_request_from_mirror_or_cache))
                .await
            {
                error!("Failed to serve connection: {:?}", err);
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

fn write_to_cache(
    temp_filename: PathBuf,
    cached_file_path: PathBuf,
) -> Box<dyn Fn(&[u8]) -> () + Send + Sync> {
    Box::new(move |data: &[u8]| {
        if data.len() > 0 {
            std::fs::create_dir_all(temp_filename.parent().unwrap()).unwrap();
            let mut file = OpenOptions::new()
                .write(true)
                .append(true)
                .create(true)
                .open(temp_filename.clone())
                .unwrap();
            file.write_all(data).unwrap();
        } else {
            debug!(
                "Renaming temp file '{}' to cached file '{}'.",
                temp_filename.display(),
                cached_file_path.display()
            );
            std::fs::create_dir_all(cached_file_path.parent().unwrap()).unwrap();
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(cached_file_path.clone());
            if file.is_ok() {
                std::fs::rename(temp_filename.clone(), cached_file_path.clone()).unwrap();
            } else {
                debug!("File already exists in cache: {:?}", cached_file_path);
                debug!("Discarding temporary file: {:?}", temp_filename);
                std::fs::remove_file(temp_filename.clone()).unwrap();
            }
        }

        ()
    })
}

async fn stream_request_from_mirror_or_cache(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<BoxBody<Bytes, std::io::Error>>> {
    let tmp_dir = Path::new("./tmp").canonicalize().unwrap();
    debug!("Temporary directory: {:?}", tmp_dir);
    let work_dir = Path::new("./cache").canonicalize().unwrap();
    debug!("Cache directory: {:?}", work_dir);
    let uri = req.uri();
    let cache_uri_path: String = (work_dir.to_str().unwrap().to_owned() + uri.path()).to_owned();
    debug!("{:?}", cache_uri_path);

    let cached_file_path = Path::new(&cache_uri_path).clean().to_path_buf();
    debug!("{:?}", cached_file_path);
    if !cached_file_path.starts_with(work_dir) {
        debug!("Cached file path is not in cache directory.");
        return Ok(not_found());
    }
    let status: StatusCode;

    if cached_file_path.exists() && !uri.to_string().ends_with("/") {
        debug!("Cached file exists: {:?}", cached_file_path);
        let file = File::open(cached_file_path).await;
        if file.is_err() {
            error!("ERROR: Unable to open file.");
            return Ok(not_found());
        }

        let file: File = file.unwrap();

        let reader_stream = ReaderStream::with_capacity(file, STREAM_BUFFER_SIZE);
        let stream_body = StreamBody::new(reader_stream.map_ok(Frame::data));

        let boxed_body = stream_body.boxed();
        let response = Response::builder()
            .status(StatusCode::OK)
            .body(boxed_body)
            .unwrap();
        Ok(response)
    } else {
        debug!("{:?}", req);
        let resp = reqwest::get(
            "http://archlinux.mirror.digitalpacific.com.au".to_owned() + &uri.to_string(),
        )
        .await
        .unwrap();
        debug!("{resp:#?}");
        status = resp.status().clone();
        if status.is_success() && !uri.to_string().ends_with("/") {
            let stream = resp.bytes_stream().map_err(std::io::Error::other);
            let async_stream = tokio_util::io::StreamReader::new(stream);
            let temp_name: String = repeat_with(fastrand::alphanumeric).take(12).collect();
            let temp_filename = tmp_dir.join(PathBuf::from(temp_name));
            let curried_write_to_cache = write_to_cache(temp_filename, cached_file_path);
            let reader_inspector = InspectReader::new(async_stream, curried_write_to_cache);
            let reader_stream = ReaderStream::new(reader_inspector);
            let stream_body = StreamBody::new(reader_stream.map_ok(Frame::data));
            let boxed_body = stream_body.boxed();

            let response = Response::builder()
                .status(StatusCode::OK)
                .body(boxed_body)
                .unwrap();
            Ok(response)
        } else {
            let stream = resp.bytes_stream().map_err(std::io::Error::other);
            let stream_body = StreamBody::new(stream.map_ok(Frame::data));
            let boxed_body = stream_body.boxed();

            debug!("{:?}", status);
            let response = Response::builder().status(status).body(boxed_body).unwrap();
            Ok(response)
        }
    }
}
