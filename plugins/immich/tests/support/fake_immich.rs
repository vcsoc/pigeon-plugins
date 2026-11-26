//! Loopback fake of the Immich API used by the plugin tests. Thread per connection, so one stalled request
//! (a gate) never delays the others. Never contacts a real server.

use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

pub const ASSET: &str = "11111111-1111-4111-8111-111111111111";
pub const SECOND: &str = "33333333-3333-4333-8333-333333333333";
pub const THIRD: &str = "44444444-4444-4444-8444-444444444444";
pub const TAG: &str = "22222222-2222-4222-8222-222222222222";
pub const PASSWORD: &str = "fixture-password";

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: &'static str,
    location: Option<&'static str>,
}

impl Response {
    pub fn json(value: Value) -> Response {
        Response { status: 200, body: value.to_string().into_bytes(), content_type: "application/json", location: None }
    }
    pub fn status(status: u16) -> Response {
        Response { status, body: b"{}".to_vec(), content_type: "application/json", location: None }
    }
    pub fn bytes(body: Vec<u8>, content_type: &'static str) -> Response {
        Response { status: 200, body, content_type, location: None }
    }
    pub fn redirect(location: &'static str) -> Response {
        Response { location: Some(location), ..Response::status(302) }
    }
}

pub type Handler = Arc<dyn Fn(&Recorded) -> Response + Send + Sync>;

pub struct FakeImmich {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeImmich {
    pub fn start(handler: Handler) -> FakeImmich {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (record, done) = (requests.clone(), stop.clone());
        let thread = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let handler = handler.clone();
                        let record = record.clone();
                        thread::spawn(move || serve(stream, handler, record));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(2)),
                    Err(_) => break,
                }
            }
        });
        FakeImmich { url, requests, stop, thread: Some(thread) }
    }

    pub fn recorded(&self) -> Vec<Recorded> {
        self.requests.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

impl Drop for FakeImmich {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(stream: TcpStream, handler: Handler, record: Arc<Mutex<Vec<Recorded>>>) {
    // macOS hands accepted sockets the listener's O_NONBLOCK flag; reads must block here.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    if reader.read_line(&mut first).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; length.min(64 * 1024 * 1024)];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let request = Recorded { method, path, headers, body };
    if let Ok(mut log) = record.lock() {
        log.push(request.clone());
    }
    let response = handler(&request);
    let mut stream = reader.into_inner();
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        if response.status == 200 { "OK" } else { "ERR" },
        response.content_type,
        response.body.len()
    );
    if let Some(location) = response.location {
        head.push_str(&format!("Location: {location}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&response.body);
}

/// Extracts the `assetData` file part of a multipart upload.
pub fn multipart_file(body: &[u8], content_type: &str) -> Vec<u8> {
    let boundary = content_type.split("boundary=").nth(1).unwrap_or("").trim_matches('"').to_owned();
    let marker = format!("--{boundary}");
    let text_start = body.windows(b"name=\"assetData\"".len()).position(|w| w == b"name=\"assetData\"").unwrap_or(0);
    let after = &body[text_start..];
    let data_start = after.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4).unwrap_or(0);
    let rest = &after[data_start..];
    let end = rest
        .windows(marker.len() + 2)
        .position(|w| w == format!("\r\n{marker}").as_bytes())
        .unwrap_or(rest.len());
    rest[..end].to_vec()
}

pub fn sha1_hex(bytes: &[u8]) -> String {
    Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A complete, well-behaved Immich: two pages of images, one tag, originals and previews.
/// `originals` maps asset id to the original bytes. Uploads are stored in `uploads` (keyed by checksum).
pub struct Fixture {
    pub originals: HashMap<String, Vec<u8>>,
    pub preview: Vec<u8>,
    pub uploads: Mutex<HashMap<String, Vec<u8>>>,
    pub pigeon_metadata: Mutex<HashMap<String, Value>>,
    pub fail_tag_creation: AtomicBool,
    pub fail_metadata_read: AtomicBool,
    /// Serve originals whose bytes do not match the advertised checksum.
    pub corrupt_original: AtomicBool,
    /// Requests whose path contains the key wait for the sender (once per entry) before being answered.
    pub holds: Mutex<Vec<(String, std::sync::mpsc::Receiver<()>)>>,
    /// Requests whose path contains the key wait while the flag is set (any number of requests).
    pub gates: Mutex<Vec<(String, Arc<AtomicBool>)>>,
}

pub fn standard_handler(fixture: Arc<Fixture>) -> Handler {
    Arc::new(move |request: &Recorded| {
        let held = {
            let mut holds = fixture.holds.lock().unwrap_or_else(|p| p.into_inner());
            holds.iter().position(|(key, _)| request.path.contains(key.as_str())).map(|i| holds.remove(i))
        };
        if let Some((_, release)) = held {
            let _ = release.recv_timeout(Duration::from_secs(20));
        }
        let gate = fixture.gates.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(key, _)| request.path.contains(key.as_str())).map(|(_, flag)| flag.clone());
        if let Some(flag) = gate {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while flag.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
        }
        let path = request.path.split('?').next().unwrap_or("").to_owned();
        let auth = request.headers.get("authorization").cloned().unwrap_or_default();
        if request.path == "/api/auth/login" {
            return if request.json()["password"] == PASSWORD {
                Response::json(json!({"accessToken":"fixture-token"}))
            } else {
                Response::status(401)
            };
        }
        if path == "/api/server/version" {
            return Response::json(json!({"major":2,"minor":5,"patch":0}));
        }
        if !auth.starts_with("Bearer ") && !request.headers.contains_key("x-api-key") {
            return Response::status(401);
        }
        match (request.method.as_str(), path.as_str()) {
            ("POST", "/api/search/metadata") => {
                let page = request.json()["page"].as_u64().unwrap_or(1);
                let items = match page {
                    1 => vec![asset_json(ASSET, "2025-03-04T10:00:00", "a.png", true), asset_json(SECOND, "bad-date", "b.png", false)],
                    _ => vec![asset_json(THIRD, "2024-12-31T23:00:00", "c.jpg", false)],
                };
                let next = if page == 1 { json!("2") } else { Value::Null };
                Response::json(json!({"assets":{"items":items,"nextPage":next}}))
            }
            ("GET", "/api/tags") => Response::json(json!([{"id":TAG,"value":"travel"}])),
            ("PUT", "/api/tags") => {
                if fixture.fail_tag_creation.load(Ordering::SeqCst) {
                    return Response::status(500);
                }
                let names: Vec<String> = request.json()["tags"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
                let out: Vec<Value> = names
                    .iter()
                    .map(|n| json!({"id": if n == "travel" { TAG.to_owned() } else { format!("55555555-5555-4555-8555-{:012}", n.len()) }, "value": n}))
                    .collect();
                Response::json(json!(out))
            }
            ("PUT", p) if p.starts_with("/api/tags/") && p.ends_with("/assets") => {
                let ids = request.json()["ids"].clone();
                let out: Vec<Value> = ids.as_array().into_iter().flatten().map(|id| json!({"id":id,"success":true})).collect();
                Response::json(json!(out))
            }
            ("GET", p) if p.ends_with("/thumbnail") => Response::bytes(fixture.preview.clone(), "image/jpeg"),
            ("GET", p) if p.ends_with("/original") => {
                let id = p.trim_start_matches("/api/assets/").trim_end_matches("/original");
                match fixture.originals.get(id) {
                    Some(bytes) => {
                        let mut bytes = bytes.clone();
                        if fixture.corrupt_original.load(Ordering::SeqCst) {
                            bytes[0] ^= 0xff;
                        }
                        Response::bytes(bytes, "application/octet-stream")
                    }
                    None => Response::status(404),
                }
            }
            ("GET", p) if p.ends_with("/metadata/pigeon") => {
                if fixture.fail_metadata_read.load(Ordering::SeqCst) {
                    return Response::status(500);
                }
                let id = p.trim_start_matches("/api/assets/").trim_end_matches("/metadata/pigeon");
                let value = fixture.pigeon_metadata.lock().ok().and_then(|m| m.get(id).cloned()).unwrap_or(Value::Null);
                Response::json(json!({"key":"pigeon","value":value}))
            }
            ("GET", p) if p.ends_with("/metadata") => {
                let id = p.trim_start_matches("/api/assets/").trim_end_matches("/metadata");
                let value = fixture.pigeon_metadata.lock().ok().and_then(|m| m.get(id).cloned()).unwrap_or(json!({}));
                Response::json(json!([{"key":"pigeon","value":value}]))
            }
            ("PUT", p) if p.ends_with("/metadata") => {
                let id = p.trim_start_matches("/api/assets/").trim_end_matches("/metadata").to_owned();
                let value = request.json()["items"][0]["value"].clone();
                if let Ok(mut m) = fixture.pigeon_metadata.lock() {
                    m.insert(id, value);
                }
                Response::json(json!({}))
            }
            ("GET", p) if p.starts_with("/api/assets/") => {
                let id = p.trim_start_matches("/api/assets/").to_owned();
                match fixture.originals.get(&id) {
                    Some(bytes) => Response::json(json!({
                        "id": id, "originalFileName": file_name(&id), "checksum": sha1_hex(bytes),
                        "fileModifiedAt": "2024-01-02T03:04:05.000Z", "fileCreatedAt": "2024-01-02T03:04:05.000Z",
                        "tags":[{"value":"remote"}], "exifInfo":{"description":"remote note","rating":3,"fileSizeInByte":bytes.len()}
                    })),
                    None => Response::status(404),
                }
            }
            ("PUT", p) if p.starts_with("/api/assets/") => Response::json(json!({})),
            ("POST", "/api/assets") => {
                let content_type = request.headers.get("content-type").cloned().unwrap_or_default();
                let file = multipart_file(&request.body, &content_type);
                let checksum = sha1_hex(&file);
                let id = ASSET.to_owned();
                if let Ok(mut uploads) = fixture.uploads.lock() {
                    uploads.insert(checksum.clone(), file.clone());
                }
                if let Ok(mut m) = fixture.pigeon_metadata.lock() {
                    m.remove(&id);
                }
                Response::json(json!({"id":id,"status":"created"}))
            }
            _ => Response::status(404),
        }
    })
}

pub fn asset_json(id: &str, local: &str, name: &str, favorite: bool) -> Value {
    json!({"id":id,"type":"IMAGE","originalFileName":name,"localDateTime":local,"isFavorite":favorite,
           "tags":[{"id":TAG,"value":"travel"}],"exifInfo":{"fileSizeInByte":1234,"exifImageWidth":4,"exifImageHeight":3}})
}

/// A small valid JPEG for previews.
pub fn jpeg_preview() -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(64, 48, image::Rgb([200, 120, 40]))
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .expect("encode fixture jpeg");
    out.into_inner()
}

pub fn new_fixture() -> Arc<Fixture> {
    Arc::new(Fixture {
        originals: HashMap::from([
            (ASSET.to_owned(), b"original-bytes-one".to_vec()),
            (SECOND.to_owned(), b"original-bytes-two".to_vec()),
            (THIRD.to_owned(), b"original-bytes-three".to_vec()),
        ]),
        preview: jpeg_preview(),
        uploads: Mutex::new(HashMap::new()),
        pigeon_metadata: Mutex::new(HashMap::new()),
        fail_tag_creation: AtomicBool::new(false),
        fail_metadata_read: AtomicBool::new(false),
        corrupt_original: AtomicBool::new(false),
        holds: Mutex::new(Vec::new()),
        gates: Mutex::new(Vec::new()),
    })
}

/// Original file name of each fixture asset.
pub fn file_name(id: &str) -> &'static str {
    match id {
        SECOND => "second.png",
        THIRD => "third.png",
        _ => "fixture.png",
    }
}
