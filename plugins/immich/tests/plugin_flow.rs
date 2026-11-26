//! Behavioural tests of the real `pigeon-immich` process against a loopback fake Immich, driven through the
//! mock host over protocol v2 stdio. Covers: handshake and manifest, preferences (remember / session-only /
//! wrong password), the collection tree and list, thumbnails, verified downloads and uploads, stop/retry,
//! tag write-back and the tag picker, sync cancellation, and the non-blocking proofs (stalled server,
//! flooded queue, saturated workers, slow login).

mod support;

use pigeon_immich::api::{ImmichClient, MAX_BYTES};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc, Arc};
use std::time::{Duration, Instant};
use support::fake_immich::*;
use support::mock_host::*;

const SETTINGS: &str = "immich/settings";
const STATUS: &str = "immich/status";

struct Env {
    _dir: tempfile::TempDir,
    data: PathBuf,
    session: PathBuf,
    server: FakeImmich,
    fixture: Arc<Fixture>,
}

fn env() -> Env {
    env_with_handler(|handler| handler)
}

fn env_with_handler(wrap: impl FnOnce(Handler) -> Handler) -> Env {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("data");
    let session = dir.path().join("session");
    let fixture = new_fixture();
    let server = FakeImmich::start(wrap(standard_handler(fixture.clone())));
    Env { _dir: dir, data, session, server, fixture }
}

fn host(env: &Env) -> MockHost {
    MockHost::start(&env.data, &env.session)
}

/// Fills the preferences form like a user would and clicks Connect / save.
fn connect(host: &mut MockHost, url: &str, remember: bool) {
    host.open(SETTINGS);
    host.event(SETTINGS, "enabled", "change", json!(true), 1);
    host.event(SETTINGS, "server", "change", json!(url), 2);
    host.event(SETTINGS, "email", "change", json!("fixture@example.test"), 3);
    host.event(SETTINGS, "password", "change", json!(PASSWORD), 4);
    host.event(SETTINGS, "remember", "change", json!(remember), 5);
    host.event(SETTINGS, "connect", "click", Value::Null, 0);
    let connected = host.until(Duration::from_secs(20), |h| settings_text(h).contains("Connected:"));
    assert!(connected, "connect did not complete: {}", settings_text(host));
}

fn settings_text(host: &MockHost) -> String {
    host.scenes.get(SETTINGS).map(scene_text).unwrap_or_default()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Waits until a modal scene contains `needle`.
fn wait_scene_text(host: &mut MockHost, instance: &str, needle: &str) -> bool {
    host.until(Duration::from_secs(20), |h| h.scenes.get(instance).map(scene_text).unwrap_or_default().contains(needle))
}

fn batch_instance(host: &MockHost) -> String {
    host.modals
        .iter()
        .rev()
        .find(|m| m["contribution"] == "transfer")
        .and_then(|m| m["instance"].as_str().map(str::to_owned))
        .expect("a transfer dialog was opened")
}

fn wait_idle(host: &mut MockHost) {
    assert!(host.until(Duration::from_secs(20), |h| {
        let text = settings_text(h);
        !text.contains("Connecting to Immich") && !text.contains("Syncing Immich") && !text.contains("Testing the connection")
    }));
}

#[test]
fn handshake_advertises_exactly_the_manifest_contributions() {
    let env = env();
    let host = host(&env);
    let manifest: Value = serde_json::from_str(&read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin.json"))).unwrap();
    assert_eq!(host.ready["plugin"]["id"], "immich");
    assert_eq!(host.ready["protocol"], 2);
    assert_eq!(host.ready["contributions"], manifest["ui"]["contributions"], "static manifest and ready message must match");
    assert_eq!(manifest["ui"]["contributions"], pigeon_immich::contributions());
    let permissions: Vec<String> = manifest["ui"]["permissions"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_owned()).collect();
    assert_eq!(permissions, pigeon_immich::PERMISSIONS.to_vec());
}

#[test]
fn ping_is_answered_while_the_plugin_is_idle() {
    let env = env();
    let mut host = host(&env);
    let rtt = host.ping(Duration::from_secs(2)).expect("pong");
    assert!(rtt < Duration::from_millis(500), "idle ping took {rtt:?}");
}

#[test]
fn preferences_remember_stores_only_a_keychain_reference_on_disk() {
    let env = env();
    let mut host = host(&env);
    let scene = host.open(SETTINGS);
    assert!(find_node(&scene, "connect").is_some());
    assert!(find_node(&scene, "password").unwrap()["password"] == true);
    connect(&mut host, &env.server.url, true);
    let secrets = host.secrets.lock().unwrap().clone();
    assert_eq!(secrets.len(), 1, "one keychain entry for the remembered account");
    assert!(secrets.values().any(|v| v == PASSWORD));
    let on_disk = read(&env.data.join("immich.json"));
    assert!(on_disk.contains("secretId"));
    assert!(!on_disk.contains(PASSWORD), "password must never be written to disk");
    assert!(!on_disk.to_lowercase().contains("password"));
    assert!(env.server.recorded().iter().any(|r| r.path == "/api/auth/login"));
    assert!(settings_text(&host).contains("Connected: 3 images, 1 tags"));
}

#[test]
fn session_only_login_never_touches_the_keychain_or_the_disk() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, false);
    assert!(host.secrets.lock().unwrap().is_empty(), "session-only must not store the password");
    // Like the built-in client, a session-only login is kept in memory: nothing is written.
    assert!(!env.data.join("immich.json").exists());
    assert!(!read(&env.data.join("immich.json")).contains(PASSWORD));
}

#[test]
fn wrong_password_is_reported_and_nothing_is_saved() {
    let env = env();
    let mut host = host(&env);
    host.open(SETTINGS);
    host.event(SETTINGS, "enabled", "change", json!(true), 1);
    host.event(SETTINGS, "server", "change", json!(env.server.url), 2);
    host.event(SETTINGS, "email", "change", json!("fixture@example.test"), 3);
    host.event(SETTINGS, "password", "change", json!("wrong"), 4);
    host.event(SETTINGS, "remember", "change", json!(true), 5);
    host.event(SETTINGS, "connect", "click", Value::Null, 0);
    assert!(host.until(Duration::from_secs(20), |h| settings_text(h).contains("rejected (HTTP 401)")));
    assert!(host.secrets.lock().unwrap().is_empty());
    assert!(!env.data.join("immich.json").exists());
}

#[test]
fn authenticated_requests_reject_redirects() {
    let server = FakeImmich::start(Arc::new(|r| match r.path.as_str() {
        "/api/auth/login" => Response::json(json!({"accessToken":"fixture-token"})),
        "/api/tags" => Response::redirect("/redirect-target"),
        _ => Response::json(json!([])),
    }));
    let client = ImmichClient::login(&server.url, "fixture@example.test", PASSWORD).unwrap();
    assert!(client.list_tags().unwrap_err().contains("HTTP 302"));
    let requests = server.recorded();
    assert_eq!(requests.len(), 2, "the redirect destination was contacted");
    assert_eq!(requests[1].path, "/api/tags");
}

#[test]
fn responses_over_the_size_limit_are_rejected() {
    let server = FakeImmich::start(Arc::new(|r| match r.path.as_str() {
        "/api/auth/login" => Response::json(json!({"accessToken":"fixture-token"})),
        _ => Response::bytes(vec![0; MAX_BYTES as usize + 1], "image/jpeg"),
    }));
    let client = ImmichClient::login(&server.url, "fixture@example.test", PASSWORD).unwrap();
    assert!(client.thumbnail(ASSET).unwrap_err().contains("exceeds the size limit"));
}

#[test]
fn expired_sessions_retry_once_only_when_credentials_are_remembered() {
    for remember in [true, false] {
        let env = env_with_handler(|standard| {
            let logins = AtomicUsize::new(0);
            Arc::new(move |r| {
                if r.path == "/api/auth/login" && r.json()["password"] == PASSWORD {
                    let login = logins.fetch_add(1, Ordering::SeqCst);
                    return Response::json(json!({"accessToken": format!("fixture-{login}")}));
                }
                if r.path.contains("/thumbnail") && r.headers.get("authorization").is_some_and(|a| a == "Bearer fixture-0") {
                    return Response::status(401);
                }
                standard(r)
            })
        });
        let mut host = host(&env);
        connect(&mut host, &env.server.url, remember);
        let response = host.request("source.thumbnail", json!({"source":"immich","id":ASSET}));
        if remember {
            assert!(response["result"]["file"].is_string(), "{response}");
        } else {
            assert!(response["result"]["error"].as_str().is_some_and(|e| e.contains("session-only")), "{response}");
        }
        let requests = env.server.recorded();
        let logins = requests.iter().filter(|r| r.path == "/api/auth/login").count();
        let previews: Vec<_> = requests.iter().filter(|r| r.path.contains("/thumbnail"))
            .map(|r| r.headers["authorization"].as_str()).collect();
        if remember {
            assert_eq!(logins, 2);
            assert_eq!(previews, ["Bearer fixture-0", "Bearer fixture-1"]);
        } else {
            assert_eq!(logins, 1);
            assert_eq!(previews, ["Bearer fixture-0"]);
        }
    }
}

#[test]
fn test_connection_logs_in_without_saving_anything() {
    let env = env();
    let mut host = host(&env);
    host.open(SETTINGS);
    host.event(SETTINGS, "server", "change", json!(env.server.url), 1);
    host.event(SETTINGS, "email", "change", json!("fixture@example.test"), 2);
    host.event(SETTINGS, "password", "change", json!(PASSWORD), 3);
    host.event(SETTINGS, "test", "click", Value::Null, 0);
    assert!(host.until(Duration::from_secs(20), |h| settings_text(h).contains("Nothing was saved")));
    assert!(host.secrets.lock().unwrap().is_empty());
    assert!(!env.data.join("immich.json").exists(), "a test connection saves no settings");
}

#[test]
fn collection_tree_lists_root_annual_years_and_twelve_months() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let describe = host.request("source.describe", json!({"source":"immich"}));
    assert_eq!(describe["result"]["root"]["id"], "root");
    let caps: Vec<&str> = describe["result"]["capabilities"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    assert!(caps.contains(&"browse") && caps.contains(&"download") && !caps.contains(&"delete"));

    let root = host.request("source.children", json!({"source":"immich","node":"root"}));
    assert_eq!(root["result"]["nodes"][0]["id"], "annual");
    assert_eq!(root["result"]["nodes"][0]["title"], "Annual");

    let annual = host.request("source.children", json!({"source":"immich","node":"annual"}));
    let years: Vec<&str> = annual["result"]["nodes"].as_array().unwrap().iter().map(|n| n["title"].as_str().unwrap()).collect();
    assert_eq!(years, vec!["2025", "2024"], "years newest first");

    let months = host.request("source.children", json!({"source":"immich","node":"y:2025"}));
    let nodes = months["result"]["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 12, "all twelve months are present, as in the built-in tree");
    assert_eq!(nodes[2]["title"], "March");
    assert_eq!(nodes[2]["count"], 1);
    assert_eq!(nodes[0]["count"], 0);
}

#[test]
fn list_shows_invalid_dates_at_root_and_pages_by_cursor() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let root = host.request("source.list", json!({"source":"immich","node":"root","limit":2}));
    let items = root["result"]["items"].as_array().unwrap();
    assert_eq!(root["result"]["total"], 3);
    assert_eq!(items.len(), 2);
    assert!(root["result"]["next"].is_string(), "more pages are advertised");
    let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&SECOND), "the invalid-date image is still available at the root");
    let page2 = host.request("source.list", json!({"source":"immich","node":"root","limit":2,"cursor":root["result"]["next"]}));
    assert_eq!(page2["result"]["items"].as_array().unwrap().len(), 1);
    assert!(page2["result"].get("next").is_none());

    let march = host.request("source.list", json!({"source":"immich","node":"m:2025:03"}));
    assert_eq!(march["result"]["total"], 1);
    assert_eq!(march["result"]["items"][0]["id"], ASSET);
    assert_eq!(march["result"]["items"][0]["meta"]["originalAvailable"], true);
}

#[test]
fn thumbnails_come_from_the_preview_endpoint_and_are_cached_privately() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let first = host.request("source.thumbnail", json!({"source":"immich","id":ASSET,"size":256}));
    let file = PathBuf::from(first["result"]["file"].as_str().expect("file in session dir"));
    assert!(file.starts_with(&env.session), "thumbnail handed over in the session folder");
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(&bytes[..2], &[0xff, 0xd8], "a JPEG preview");
    let account_dir = std::fs::read_dir(env.data.join("thumbs")).unwrap().next().unwrap().unwrap().path();
    let cached = account_dir.join(format!("{ASSET}.jpg"));
    assert!(cached.exists(), "cached in the account folder");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&account_dir).unwrap().permissions().mode() & 0o777, 0o700);
    }
    let previews_before = env.server.recorded().iter().filter(|r| r.path.contains("thumbnail")).count();
    host.request("source.thumbnail", json!({"source":"immich","id":ASSET,"size":256}));
    let previews_after = env.server.recorded().iter().filter(|r| r.path.contains("thumbnail")).count();
    assert_eq!(previews_before, previews_after, "a cached preview is not fetched again");
}

#[test]
fn originals_are_never_opened_for_editing_only_previews() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let edit = host.request("source.open", json!({"source":"immich","id":ASSET,"purpose":"edit"}));
    assert!(edit["error"]["message"].as_str().unwrap().contains("thumbnail-only"));
    let view = host.request("source.open", json!({"source":"immich","id":ASSET,"purpose":"view"}));
    assert!(view["result"]["file"].is_string());
    assert!(!env.server.recorded().iter().any(|r| r.path.ends_with("/original")), "no original is fetched for browsing");
}

#[test]
fn download_verifies_the_checksum_writes_a_sidecar_and_indexes_the_file() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let folder = env.data.parent().unwrap().join("downloads");
    std::fs::create_dir_all(&folder).unwrap();
    host.pick_folder = Some(folder.to_string_lossy().into_owned());
    host.import_ids = vec!["local-1".into()];
    host.send(json!({"v":2,"t":"command","command":"download","context":{"ids":[ASSET]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.modals.iter().any(|m| m["contribution"] == "transfer")));
    let instance = batch_instance(&host);
    assert!(wait_scene_text(&mut host, &instance, "All originals and metadata verified"));
    assert_eq!(std::fs::read(folder.join("fixture.png")).unwrap(), b"original-bytes-one");
    let sidecars: Vec<_> = std::fs::read_dir(&folder).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains(".pigeon-immich.")).collect();
    assert_eq!(sidecars.len(), 1, "one immutable sidecar: {sidecars:?}");
    let sidecar: Value = serde_json::from_str(&read(&folder.join(&sidecars[0]))).unwrap();
    assert_eq!(sidecar["schema"], "pigeon.immich-download/v1");
    assert_eq!(sidecar["assetId"], ASSET);
    let calls: Vec<&str> = host.calls.iter().filter_map(|c| c["method"].as_str()).collect();
    assert!(calls.contains(&"library.import"), "the file is indexed through the host");
    assert!(calls.contains(&"library.set_tags") && calls.contains(&"library.set_meta"));
    let set_meta = host.calls.iter().find(|c| c["method"] == "library.set_meta").unwrap();
    assert_eq!(set_meta["params"]["patch"]["note"], "remote note");
    assert_eq!(set_meta["params"]["patch"]["rating"], 3);
    assert!(host.progress.iter().any(|p| p["method"] == "progress.end"), "progress job ends");
    assert_eq!(env.server.recorded().iter().filter(|r| r.path.ends_with("/original")).count(), 1);
}

#[test]
fn identical_destination_is_reused_without_another_original_get() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let folder = env.data.parent().unwrap().join("reuse");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("fixture.png"), b"original-bytes-one").unwrap();
    host.pick_folder = Some(folder.to_string_lossy().into_owned());
    host.send(json!({"v":2,"t":"command","command":"download","context":{"ids":[ASSET]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.modals.iter().any(|m| m["contribution"] == "transfer")));
    let instance = batch_instance(&host);
    assert!(wait_scene_text(&mut host, &instance, "All originals and metadata verified"));
    assert_eq!(env.server.recorded().iter().filter(|r| r.path.ends_with("/original")).count(), 0);
}

#[test]
fn conflicting_file_is_left_unchanged_and_reported() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let folder = env.data.parent().unwrap().join("conflict");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("fixture.png"), b"someone else's file").unwrap();
    host.pick_folder = Some(folder.to_string_lossy().into_owned());
    host.send(json!({"v":2,"t":"command","command":"download","context":{"ids":[ASSET]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.modals.iter().any(|m| m["contribution"] == "transfer")));
    let instance = batch_instance(&host);
    assert!(wait_scene_text(&mut host, &instance, "nothing was overwritten"));
    assert_eq!(std::fs::read(folder.join("fixture.png")).unwrap(), b"someone else's file");
}

#[test]
fn corrupt_download_is_never_published() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    env.fixture.corrupt_original.store(true, Ordering::SeqCst);
    let folder = env.data.parent().unwrap().join("corrupt");
    std::fs::create_dir_all(&folder).unwrap();
    host.pick_folder = Some(folder.to_string_lossy().into_owned());
    host.send(json!({"v":2,"t":"command","command":"download","context":{"ids":[ASSET]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.modals.iter().any(|m| m["contribution"] == "transfer")));
    let instance = batch_instance(&host);
    assert!(wait_scene_text(&mut host, &instance, "checksum verification"));
    assert!(!folder.join("fixture.png").exists());
    let leftovers: Vec<_> = std::fs::read_dir(&folder).unwrap().filter_map(|e| e.ok()).collect();
    assert!(leftovers.is_empty(), "no temporary files left behind: {leftovers:?}");
}

#[test]
fn stop_finishes_the_current_file_and_retry_completes_the_rest() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let folder = env.data.parent().unwrap().join("stopretry");
    std::fs::create_dir_all(&folder).unwrap();
    host.pick_folder = Some(folder.to_string_lossy().into_owned());
    // Hold the first original so Stop can be pressed while it is in progress.
    let (release, gate) = mpsc::channel::<()>();
    env.fixture.holds.lock().unwrap().push((format!("/api/assets/{ASSET}/original"), gate));
    host.send(json!({"v":2,"t":"command","command":"download","context":{"ids":[ASSET, SECOND, THIRD]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.modals.iter().any(|m| m["contribution"] == "transfer")));
    let instance = batch_instance(&host);
    assert!(host.until(Duration::from_secs(10), |h| h.scenes.get(&instance).map(|s| scene_text(s).contains("Verifying")).unwrap_or(false)));
    host.event(&instance, "stop", "click", Value::Null, 0);
    release.send(()).unwrap();
    assert!(wait_scene_text(&mut host, &instance, "Batch incomplete"));
    let text = scene_text(&host.scenes[&instance]);
    assert!(text.contains("1 of 3 verified"), "{text}");
    assert!(text.contains("Not started; the batch was stopped"));
    assert!(folder.join("fixture.png").exists());
    host.event(&instance, "retry", "click", Value::Null, 0);
    assert!(wait_scene_text(&mut host, &instance, "All originals and metadata verified"));
    let downloads = std::fs::read_dir(&folder).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".png")).count();
    assert_eq!(downloads, 3, "each original is published once, under its own name");
}

#[test]
fn upload_sends_local_files_and_reads_back_the_pigeon_metadata() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let local = env.data.parent().unwrap().join("local.png");
    std::fs::write(&local, b"original-bytes-one").unwrap();
    host.library_records = vec![json!({"id":"local-1","name":"local.png","path":local,"tags":["trip"],"note":"hello","rating":4,"favorite":true})];
    host.send(json!({"v":2,"t":"command","command":"send","context":{"ids":["local-1"]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.modals.iter().any(|m| m["contribution"] == "transfer")));
    let instance = batch_instance(&host);
    assert!(wait_scene_text(&mut host, &instance, "All originals and metadata verified"));
    let uploads = env.fixture.uploads.lock().unwrap();
    assert_eq!(uploads.values().next().map(|v| v.as_slice()), Some(&b"original-bytes-one"[..]), "the original bytes were sent unchanged");
    let stored = env.fixture.pigeon_metadata.lock().unwrap();
    let value = stored.get(ASSET).expect("metadata written");
    assert_eq!(value["note"], "hello");
    assert_eq!(value["tags"], json!(["trip"]));
    assert!(!value.to_string().contains(&local.to_string_lossy().to_string()), "local paths never leave the machine");
}

#[test]
fn upload_is_not_a_success_when_metadata_cannot_be_read_back() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    env.fixture.fail_metadata_read.store(true, Ordering::SeqCst);
    let local = env.data.parent().unwrap().join("partial.png");
    std::fs::write(&local, b"original-bytes-one").unwrap();
    host.library_records = vec![json!({"id":"local-2","name":"partial.png","path":local})];
    host.send(json!({"v":2,"t":"command","command":"send","context":{"ids":["local-2"]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.modals.iter().any(|m| m["contribution"] == "transfer")));
    let instance = batch_instance(&host);
    assert!(wait_scene_text(&mut host, &instance, "retry to finish"));
    assert!(!scene_text(&host.scenes[&instance]).contains("All originals and metadata verified"));
}

#[test]
fn unknown_local_items_are_reported_per_item() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    host.library_records = vec![];
    host.send(json!({"v":2,"t":"command","command":"send","context":{"ids":["missing-id"]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.modals.iter().any(|m| m["contribution"] == "transfer")));
    let instance = batch_instance(&host);
    assert!(wait_scene_text(&mut host, &instance, "Not found in the library"));
}

#[test]
fn tag_write_back_failure_is_reported_never_silent() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    env.fixture.fail_tag_creation.store(true, Ordering::SeqCst);
    let response = host.request("source.write_meta", json!({"source":"immich","ids":[ASSET],"tags":["travel","new"]}));
    assert_eq!(response["result"]["queued"], 1);
    assert!(host.until(Duration::from_secs(20), |h| h.toast_text().iter().any(|t| t.contains("Immich tag sync failed"))));
}

#[test]
fn tag_write_back_success_is_recorded_locally() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let response = host.request("source.write_meta", json!({"source":"immich","ids":[ASSET],"tags":["travel"]}));
    assert_eq!(response["result"]["queued"], 1);
    assert!(host.until(Duration::from_secs(20), |_| env.server.recorded().iter().any(|r| r.path == format!("/api/tags/{TAG}/assets"))));
}

#[test]
fn tag_picker_assigns_an_existing_tag_to_the_selected_references() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    host.send(json!({"v":2,"t":"command","command":"tag","context":{"ids":[ASSET]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.modals.iter().any(|m| m["contribution"] == "tag-picker")));
    let instance = host.modals.iter().rev().find(|m| m["contribution"] == "tag-picker").unwrap()["instance"].as_str().unwrap().to_owned();
    assert!(host.until(Duration::from_secs(10), |h| h.scenes.contains_key(&instance)));
    host.event(&instance, "tag", "change", json!(TAG), 1);
    host.event(&instance, "apply", "click", Value::Null, 0);
    let done = host.until(Duration::from_secs(20), |h| h.scenes.get(&instance).map(scene_text).unwrap_or_default().contains("already has that tag"));
    assert!(done, "picker scene: {}; toasts: {:?}", host.scenes.get(&instance).map(scene_text).unwrap_or_default(), host.toast_text());
}

#[test]
fn tag_picker_refuses_references_outside_the_connected_account() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let response = host.request("source.children", json!({"source":"immich","node":"nope"}));
    assert!(response["error"].is_object(), "unknown collections are errors");
    host.send(json!({"v":2,"t":"command","command":"tag","context":{"ids":["99999999-9999-4999-8999-999999999999"]}}));
    assert!(host.until(Duration::from_secs(20), |h| h.toast_text().iter().any(|t| t.contains("not part of the current Immich session"))));
}

#[test]
fn sync_can_be_cancelled_by_the_host_without_changing_the_catalogue() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let (release, gate) = mpsc::channel::<()>();
    env.fixture.holds.lock().unwrap().push(("/api/search/metadata".into(), gate));
    // Only the job started by this sync counts (the connect above also started one).
    let before = host.progress.len();
    host.send(json!({"v":2,"t":"command","command":"sync","context":{}}));
    assert!(host.until(Duration::from_secs(10), |h| h.progress[before..].iter().any(|p| p["method"] == "progress.start")));
    let job = host.progress[before..].iter().find(|p| p["method"] == "progress.start").unwrap()["job"].as_str().unwrap().to_owned();
    host.send(json!({"v":2,"t":"cancel","job":job}));
    release.send(()).unwrap();
    assert!(host.until(Duration::from_secs(20), |h| h.progress.iter().any(|p| p["method"] == "progress.end")), "the cancelled sync ends");
    assert!(host.until(Duration::from_secs(20), |h| settings_text(h).contains("Immich sync cancelled")), "{}", settings_text(&host));
    let root = host.request("source.list", json!({"source":"immich","node":"root"}));
    assert_eq!(root["result"]["total"], 3, "the old catalogue is kept");
}

// ----- non-blocking proofs -------------------------------------------------------------------------------

#[test]
fn a_stalled_thumbnail_never_blocks_pings_browsing_or_the_preferences_page() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    let (release, gate) = mpsc::channel::<()>();
    env.fixture.holds.lock().unwrap().push(("/thumbnail".into(), gate));
    host.send(json!({"v":2,"t":"req","id":"stalled","method":"source.thumbnail","params":{"source":"immich","id":ASSET}}));
    std::thread::sleep(Duration::from_millis(100));
    let rtt = host.ping(Duration::from_secs(2)).expect("pong while a thumbnail hangs");
    assert!(rtt < Duration::from_millis(300), "ping took {rtt:?} while the server stalls");
    let started = Instant::now();
    let children = host.request_timeout("source.children", json!({"source":"immich","node":"annual"}), Duration::from_secs(3));
    assert!(children.is_some() && started.elapsed() < Duration::from_secs(1), "browsing still answers in {:?}", started.elapsed());
    let started = Instant::now();
    host.open(SETTINGS);
    assert!(started.elapsed() < Duration::from_secs(1), "preferences still render in {:?}", started.elapsed());
    release.send(()).unwrap();
    let done = host.wait_for(|m| m["t"] == "res" && m["id"] == "stalled", Duration::from_secs(20));
    assert!(done.is_some(), "the stalled request completes once the server answers");
}

#[test]
fn a_hung_login_keeps_the_host_responsive() {
    let env = env();
    let mut host = host(&env);
    let (release, gate) = mpsc::channel::<()>();
    env.fixture.holds.lock().unwrap().push(("/api/auth/login".into(), gate));
    host.open(SETTINGS);
    host.event(SETTINGS, "enabled", "change", json!(true), 1);
    host.event(SETTINGS, "server", "change", json!(env.server.url), 2);
    host.event(SETTINGS, "email", "change", json!("fixture@example.test"), 3);
    host.event(SETTINGS, "password", "change", json!(PASSWORD), 4);
    host.event(SETTINGS, "connect", "click", Value::Null, 0);
    assert!(host.until(Duration::from_secs(5), |h| settings_text(h).contains("Connecting")));
    let rtt = host.ping(Duration::from_secs(1)).expect("pong during the hung login");
    assert!(rtt < Duration::from_millis(300));
    let described = host.request_timeout("source.describe", json!({"source":"immich"}), Duration::from_secs(1));
    assert!(described.is_some(), "describe answers while login hangs");
    release.send(()).unwrap();
    wait_idle(&mut host);
}

#[test]
fn a_flood_of_events_keeps_the_plugin_answering_pings() {
    let env = env();
    let mut host = host(&env);
    host.open(SETTINGS);
    let mut worst = Duration::ZERO;
    let mut rev = 10u64;
    for round in 0..40 {
        for _ in 0..200 {
            rev += 1;
            host.send(json!({"v":2,"t":"ui.event","instance":SETTINGS,"node":"server","type":"change","value":format!("https://x{round}.test"),"rev":rev}));
        }
        let rtt = host.ping(Duration::from_secs(2)).expect("pong during the flood");
        worst = worst.max(rtt);
    }
    assert!(worst < Duration::from_millis(500), "worst ping during the flood: {worst:?}");
    let describe = host.request_timeout("source.describe", json!({"source":"immich"}), Duration::from_secs(5));
    assert!(describe.is_some());
}

#[test]
fn a_saturated_worker_pool_answers_busy_instead_of_hanging() {
    let env = env();
    let mut host = host(&env);
    connect(&mut host, &env.server.url, true);
    // Every preview blocks while the gate is set: the preview pool (4 threads, 256 queued) saturates.
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(true));
    env.fixture.gates.lock().unwrap().push(("/thumbnail".into(), gate.clone()));
    let total = 400u32;
    for i in 0..total {
        host.send(json!({"v":2,"t":"req","id":format!("t{i}"),"method":"source.thumbnail","params":{"source":"immich","id":ASSET}}));
    }
    let started = Instant::now();
    let mut busy = 0;
    let mut answered = 0usize;
    while answered < 150 && started.elapsed() < Duration::from_secs(10) {
        if let Some(m) = host.wait_for(|m| m["t"] == "res" && m["id"].as_str().is_some_and(|id| id.starts_with('t')), Duration::from_millis(200)) {
            answered += 1;
            if m["error"]["code"] == "busy" {
                busy += 1;
            }
        }
    }
    assert!(busy >= 100, "a flooded preview queue refuses work at once: {busy} busy answers");
    let rtt = host.ping(Duration::from_secs(2)).expect("pong while the pool is saturated");
    assert!(rtt < Duration::from_millis(500), "ping {rtt:?}");
    let children = host.request_timeout("source.children", json!({"source":"immich","node":"annual"}), Duration::from_secs(2));
    assert!(children.is_some(), "browsing is not starved by previews");
    gate.store(false, Ordering::SeqCst);
    // Every request still gets exactly one answer once the server recovers.
    let deadline = Instant::now() + Duration::from_secs(60);
    while answered < total as usize && Instant::now() < deadline {
        if host.wait_for(|m| m["t"] == "res" && m["id"].as_str().is_some_and(|id| id.starts_with('t')), Duration::from_millis(500)).is_some() {
            answered += 1;
        }
    }
    assert_eq!(answered, total as usize, "no request is lost or answered twice");
}

#[test]
fn uninstall_removes_keychain_entries_and_every_local_file() {
    let env = env();
    let shared_secrets;
    {
        let mut host = host(&env);
        connect(&mut host, &env.server.url, true);
        assert_eq!(host.secrets.lock().unwrap().len(), 1);
        shared_secrets = host.secrets.clone();
        // `shutdown` ends the session and leaves the files behind, as a normal exit does.
        host.shutdown();
    }
    assert!(env.data.join("immich.sqlite3").exists() && env.data.join("immich.json").exists());
    let secrets = shared_secrets.clone();
    let mut uninstall = MockHost::start_mode("--uninstall", &env.data, &env.session);
    uninstall.secrets = secrets.clone();
    let status = pump_until_exit(&mut uninstall, Duration::from_secs(40));
    assert!(status.success(), "uninstall exits 0: {}", uninstall.stderr());
    assert!(secrets.lock().unwrap().is_empty(), "the keychain entry is deleted through the host");
    assert!(!env.data.join("immich.sqlite3").exists());
    assert!(!env.data.join("immich.json").exists());
    assert!(!env.data.join("thumbs").exists());
}

#[test]
fn self_test_runs_without_a_server() {
    let dir = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(binary())
        .arg("--self-test")
        .env("PIGEON_PLUGIN_DATA", dir.path())
        .output()
        .expect("run self-test");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], true);
}

#[test]
fn status_bar_shows_the_connection_state() {
    let env = env();
    let mut host = host(&env);
    let off = host.open(STATUS);
    assert!(scene_text(&off).contains("Immich: off"));
    connect(&mut host, &env.server.url, true);
    let on = host.open(STATUS);
    assert!(scene_text(&on).contains("Immich: 3 images"));
}

#[test]
fn startup_reconnects_with_the_remembered_password_from_the_keychain() {
    let env = env();
    let shared_secrets;
    {
        let mut host = host(&env);
        connect(&mut host, &env.server.url, true);
        shared_secrets = host.secrets.clone();
        host.shutdown();
    }
    // A new process starts with the same keychain (the host's store), and no password typed in the UI.
    let mut again = host(&env);
    again.secrets = shared_secrets;
    assert!(
        again.until(Duration::from_secs(20), |h| h.scenes.get(STATUS).map(scene_text).unwrap_or_default().contains("Immich: 3 images")),
        "the saved connection is restored at startup"
    );
    assert!(env.server.recorded().iter().filter(|r| r.path == "/api/auth/login").count() >= 2);
}

/// Pumps host messages until the child exits (its host requests are answered meanwhile).
fn pump_until_exit(host: &mut MockHost, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = host.child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(Instant::now() < deadline, "process did not exit in time");
        let _ = host.wait_for(|_| false, Duration::from_millis(25));
    }
}
