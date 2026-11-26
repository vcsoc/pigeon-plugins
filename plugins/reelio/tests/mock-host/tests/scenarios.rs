use pigeon_mock_host::{LaunchOptions, SceneSnapshot, Schemas, Session};
use pigeon_plugin_sdk::protocol::{Incoming, KeyPhase, Mods, NodeEvent, PointerPhase};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tempfile::TempDir;

const MAIN: &str = "reelio/main";
const SUMMARY: &str = "reelio/summary";
const PROJECT: &str = "reelio/project";
const TIMEOUT: Duration = Duration::from_secs(30);
static SCHEMAS: OnceLock<Arc<Schemas>> = OnceLock::new();

struct Test {
    plugin: TempDir,
    host: Session,
}
impl Test {
    fn new() -> Self {
        Self::launch(Default::default())
    }
    fn launch(options: LaunchOptions) -> Self {
        let plugin = tempfile::tempdir().unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for file in ["run", "plugin.json"] {
            std::fs::copy(root.join(file), plugin.path().join(file)).unwrap();
        }
        std::fs::create_dir(plugin.path().join("bin")).unwrap();
        let bin = PathBuf::from(
            std::env::var_os("REELIO_TEST_BIN")
                .expect("point REELIO_TEST_BIN to the cargo-built executable"),
        )
        .canonicalize()
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(bin, plugin.path().join("bin/reelio")).unwrap();
        let host = Session::launch(
            plugin.path(),
            SCHEMAS.get_or_init(|| Arc::new(Schemas::load())).clone(),
            options,
        )
        .unwrap();
        host.wait_ready(TIMEOUT).unwrap();
        host.show(MAIN).unwrap();
        let test = Self { plugin, host };
        test.wait(MAIN, 0, |scene| scene.node("timeline.title").is_some());
        test
    }
    fn wait(
        &self,
        instance: &str,
        rev: u64,
        pred: impl Fn(&SceneSnapshot) -> bool,
    ) -> SceneSnapshot {
        self.host.wait_scene_matching(instance,rev,TIMEOUT,pred).unwrap_or_else(|err|{
            let tail=self.host.with_state(|s|s.outgoing.iter().rev().filter(|v|v["t"]!="ready").take(6).map(|v| if v["t"]=="ui.set" {let t=SceneSnapshot{revision:0,root:v["scene"].clone()};json!({"t":"ui.set","instance":v["instance"],"error":t.node("toast.error.msg"),"notice":t.node("sub.notice"),"busy":t.node("busy"),"list_count":t.node("sub:list").map(|n|n["count"].clone())})} else {v.clone()}).collect::<Vec<_>>());
            eprintln!("mock replies: {:?}",self.host.with_state(|s|s.service_replies.clone()));
            for (_,line) in self.host.violations().iter().take(3) {
                if let Ok(v)=serde_json::from_str::<Value>(line) {
                    if v["t"] == "ui.set" { invalid_leaves(SCHEMAS.get().unwrap(), &v["scene"]); }
                }
            }
            self.host.crash();
            panic!("{err}; instance={instance}; violations={:?}; recent={tail:?}; stderr={}",self.host.violations().iter().map(|(v,_)|v.to_string().chars().take(240).collect::<String>()).collect::<Vec<_>>(),self.host.with_state(|s|s.stderr_tail.clone()));
        })
    }
    fn click(&self, instance: &str, node: &str) -> u64 {
        self.host.event(instance, node, NodeEvent::Click).unwrap()
    }
    fn change(&self, instance: &str, node: &str, value: Value) -> u64 {
        let rev = self
            .host
            .scene(instance)
            .and_then(|root| {
                SceneSnapshot { revision: 0, root }
                    .node(node)
                    .and_then(|v| v["rev"].as_u64())
            })
            .unwrap_or(0);
        self.host
            .event(instance, node, NodeEvent::Change { value, rev })
            .unwrap()
    }
    fn command(&self, command: &str) {
        self.host
            .send(&Incoming::Command {
                command: command.into(),
                args: json!({}),
                context: json!({}),
            })
            .unwrap();
    }
    fn dialog(&self, paths: &[&Path]) {
        self.host.services_mut(|s| {
            s.fixtures.dialog_paths = paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        });
    }
    fn fixture(&self, name: &str) -> PathBuf {
        self.plugin.path().join(name)
    }
    fn media(&self) -> PathBuf {
        let path = self.fixture("fixture.mp4");
        let mut ffmpeg = Command::new("ffmpeg");
        ffmpeg
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=25:duration=10",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-threads",
                "1",
            ])
            .arg(&path);
        assert!(ffmpeg.status().expect("ffmpeg is required").success());
        path
    }
    fn import_video(&self, path: &Path) -> String {
        self.dialog(&[path]);
        let rev = self.click(MAIN, "project:menu");
        self.wait(MAIN, rev, |s| s.node("menu:project:open_media").is_some());
        let rev = self.click(MAIN, "menu:project:open_media");
        let scene = self.wait(MAIN, rev, |s| {
            s.by_text(path.file_name().unwrap().to_str().unwrap())
                .is_some()
                && first_canvas(&s.root).is_some()
        });
        first_canvas(&scene.root).unwrap().into()
    }
    fn modal(&self, contribution: &str) -> String {
        let until = Instant::now() + TIMEOUT;
        loop {
            if let Some(id) = self.host.services(|s| {
                s.modals
                    .iter()
                    .rev()
                    .find(|v| v["contribution"] == contribution)
                    .and_then(|v| v["instance"].as_str())
                    .map(|s| {
                        if s.contains('/') {
                            s.to_owned()
                        } else {
                            format!("reelio/{s}")
                        }
                    })
            }) {
                return id;
            }
            assert!(Instant::now() < until, "modal did not open: {contribution}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn finish(&self) {
        assert!(self.host.ping(TIMEOUT).is_some());
        assert!(self.host.shutdown(TIMEOUT));
        self.host.assert_no_violations();
    }
}
fn first_canvas(v: &Value) -> Option<&str> {
    if v["kind"] == "canvas" {
        return v["id"].as_str().filter(|id| id.starts_with("lane.canvas:"));
    }
    v["children"].as_array()?.iter().find_map(first_canvas)
}
fn invalid_leaves(schemas: &Schemas, node: &Value) {
    let mut leaf = node.clone();
    if leaf.get("children").is_some() && leaf["kind"] != "split" {
        leaf["children"] = json!([]);
    }
    if !schemas
        .check_to_host(&json!({"v":2,"t":"ui.set","instance":MAIN,"scene":leaf}))
        .is_empty()
    {
        eprintln!("invalid scene node: {leaf}");
    }
    for child in node["children"].as_array().into_iter().flatten() {
        invalid_leaves(schemas, child);
    }
}
fn pointer(phase: PointerPhase, x: f64, y: f64, button: u8) -> NodeEvent {
    NodeEvent::Pointer {
        phase,
        x: x as f32,
        y: y as f32,
        button: button.into(),
        buttons: if matches!(phase, PointerPhase::Down | PointerPhase::Move) {
            1
        } else {
            0
        },
        mods: Mods::default(),
        clicks: 1,
        pointer_id: 1,
        coalesced: 1,
    }
}

#[test]
fn preview_transport_spec() {
    let t = Test::new();
    let media = t.media();
    t.import_video(&media);
    for i in 0..10 {
        let speed = [
            "0.1x", "0.25x", "0.5x", "0.75x", "1x", "2x", "3x", "4x", "5x", "10x",
        ][i];
        let rev = t.change(MAIN, "viewer.speed", json!(i));
        t.wait(MAIN, rev, |s| {
            s.assert_node("viewer.speed.value")["text"] == speed
        });
    }
    for (node, time) in [
        ("viewer.next_frame", "00:00:00.040"),
        ("viewer.prev_frame", "00:00:00.000"),
        ("viewer.end", "00:00:10.000"),
        ("viewer.start", "00:00:00.000"),
        ("viewer.end", "00:00:10.000"),
        ("viewer.stop", "00:00:00.000"),
    ] {
        let rev = t.click(MAIN, node);
        t.wait(MAIN, rev, |s| {
            s.assert_node("viewer.clock")["text"]
                .as_str()
                .is_some_and(|v| v.starts_with(time))
        });
    }
    t.host.show(PROJECT).unwrap();
    let rev = t.click(MAIN, "nav:chapters");
    t.wait(MAIN, rev, |s| {
        s.assert_node("nav:chapters")["toggled"] == true
    });
    let rev = t.host.scene_revision(PROJECT);
    t.click(MAIN, "viewer.add_chapter");
    t.wait(PROJECT, rev, |s| s.node("chapter:title:0").is_some());
    t.finish();
}

#[test]
fn subtitle_editor_spec_and_virtualized_cues() {
    let t = Test::new();
    let source = t.fixture("source.srt");
    let cues = (0..205)
        .map(|i| {
            format!(
                "{}\n00:{:02}:{:02},000 --> 00:{:02}:{:02},800\nLoaded cue {}\n\n",
                i + 1,
                i / 60,
                i % 60,
                i / 60,
                i % 60,
                i + 1
            )
        })
        .collect::<String>();
    std::fs::write(&source, &cues).unwrap();
    let rev = t.click(MAIN, "nav:subtitles");
    t.wait(MAIN, rev, |s| s.node("sub:list").is_some());
    t.dialog(&[&source]);
    let rev = t.click(MAIN, "sub:open");
    let loaded = t.wait(MAIN, rev, |s| {
        s.node("sub:list").is_some_and(|n| n["count"] == 205)
    });
    let first = loaded.assert_node("sub:list")["window"]["rows"][0]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(first.starts_with("sub:select:"));
    let rev = t
        .host
        .event(
            MAIN,
            "sub:list",
            NodeEvent::ListWindow {
                first: 149,
                last: 155,
                overscan: 2,
            },
        )
        .unwrap();
    t.wait(MAIN, rev, |s| {
        s.assert_node("sub:list")["window"]["rows"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter().any(|r| {
                    r["label"]
                        .as_str()
                        .is_some_and(|l| l.ends_with("Loaded cue 150"))
                })
            })
    });
    let rev = t
        .host
        .event(
            MAIN,
            "sub:list",
            NodeEvent::Key {
                phase: KeyPhase::Down,
                key: "ArrowRight".into(),
                code: String::new(),
                text: String::new(),
                mods: Mods::default(),
                repeat: false,
            },
        )
        .unwrap();
    let view = t.wait(MAIN, rev, |s| {
        find_with_value(&s.root, "Loaded cue 2").is_some()
    });
    let second_id = find_with_value(&view.root, "Loaded cue 2").expect("second cue editor");
    let rev = t.change(MAIN, second_id, json!("Edited cue"));
    t.wait(MAIN, rev, |s| {
        find_with_value(&s.root, "Edited cue").is_some()
    });
    let rev = t.click(MAIN, "sub:undo");
    t.wait(MAIN, rev, |s| {
        find_with_value(&s.root, "Loaded cue 2").is_some()
    });
    let dest = t.fixture("copy.srt");
    t.dialog(&[&dest]);
    let rev = t.click(MAIN, "sub:save");
    t.wait(MAIN, rev, |s| {
        s.by_text("Saved a new SRT copy: copy.srt").is_some()
    });
    assert!(dest.is_file());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), cues);
    t.finish();
}
#[test]
fn workspace_preferences_reload_without_changing_project() {
    let mut options = LaunchOptions::default();
    options
        .fixtures
        .settings
        .insert("reelio.media-pool-width.v1".into(), json!("372"));
    options.fixtures.settings.insert(
        "reelio.workspace-layout.v1".into(),
        json!(r#"{"fullHeightLeft":true,"leftCollapsed":false,"rightCollapsed":false}"#),
    );
    let t = Test::launch(options);
    t.wait(MAIN, 0, |s| {
        s.node("pool")
            .is_some_and(|n| n["width"]["px"].as_f64() == Some(372.))
            && s.node("upper").is_none()
    });
    let rev = t.click(MAIN, "layout:full_height");
    t.wait(MAIN, rev, |s| {
        s.node("upper").is_some() && s.assert_node("undo")["enabled"] == false
    });
    t.finish();
}

#[test]
#[ignore = "explicit screenshot input and output paths"]
fn workspace_visual_fixture() {
    use pigeon_mock_host::render::{png, render_scene, Theme};
    let reference = PathBuf::from(std::env::var_os("REELIO_REFERENCE_PNG").unwrap());
    let output = PathBuf::from(std::env::var_os("REELIO_VISUAL_OUT").unwrap());
    let mut options = LaunchOptions::default();
    options
        .fixtures
        .settings
        .insert("reelio.media-pool-width.v1".into(), json!("374"));
    let t = Test::launch(options);
    t.host
        .send(&Incoming::Resize {
            instance: MAIN.into(),
            w: 1531.,
            h: 1019.,
            scale: 1.,
        })
        .unwrap();
    let cover = t.fixture("bunny.png");
    let still = t.fixture("frame.png");
    for (crop, target) in [
        ("crop=570:320:668:112", &still),
        ("crop=108:144:1667:109", &cover),
    ] {
        assert!(Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(&reference)
            .args(["-vf", crop, "-frames:v", "1"])
            .arg(target)
            .status()
            .unwrap()
            .success());
    }
    let movie = t.fixture("Big_Buck_Bunny_1080_10s_5MB.mp4");
    assert!(Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-loop",
            "1",
            "-framerate",
            "25",
            "-i"
        ])
        .arg(&still)
        .args(["-t", "10", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-threads", "1"])
        .arg(&movie)
        .status()
        .unwrap()
        .success());
    let lane = t.import_video(&movie);
    let track = lane.strip_prefix("lane.canvas:").unwrap();
    t.click(MAIN, &format!("lane.select:{track}"));
    let subtitle = t.fixture("reelio_sample_subtitle_eng.srt");
    std::fs::write(
        &subtitle,
        "1\n00:00:01,000 --> 00:00:06,400\nA subtitle sample\n",
    )
    .unwrap();
    t.host
        .event(
            MAIN,
            "pool.dropbox",
            NodeEvent::Drop {
                paths: vec![subtitle.to_string_lossy().into_owned()],
                assets: vec![],
                x: 12.,
                y: 12.,
            },
        )
        .unwrap();
    t.wait(MAIN, 0, |s| {
        s.by_text("reelio_sample_subtitle_eng.srt").is_some()
    });
    t.host.show(SUMMARY).unwrap();
    t.wait(SUMMARY, 0, |s| s.node("cover:preview").is_some());
    let rev = t
        .host
        .event(
            SUMMARY,
            "cover:preview",
            NodeEvent::Drop {
                paths: vec![cover.to_string_lossy().into_owned()],
                assets: vec![],
                x: 10.,
                y: 10.,
            },
        )
        .unwrap();
    t.wait(SUMMARY, rev, |s| {
        s.assert_node("cover:preview")["resource"] == "cover:project"
    });
    t.host
        .wait_until(
            TIMEOUT,
            |st| {
                (st.resources.contains_key("frame:current")
                    && st.resources.contains_key("cover:project"))
                .then_some(())
            },
            "frame and cover",
        )
        .unwrap();
    let (mut main, images) = t.host.render_snapshot(MAIN).unwrap();
    let mut summary = t.host.scene(SUMMARY).unwrap();
    main["width"] = json!({"px":1531});
    summary["width"] = json!({"px":379});
    main["height"] = json!({"fill":1});
    summary["height"] = json!({"fill":1});
    let combined = json!({"id":"fixture","kind":"row","children":[main,summary]});
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(t.plugin.path().join("plugin.json")).unwrap(),
    )
    .unwrap();
    let mut theme = pigeon_mock_host::render::dark_theme_protocol();
    for (name, color) in manifest["ui"]["theme_override"].as_object().unwrap() {
        theme
            .colors
            .insert(name.clone(), color.as_str().unwrap().to_owned());
    }
    let rendered = render_scene(
        &combined,
        (1910, 1046),
        &Theme::from_protocol(&theme),
        &images,
        1.,
    );
    std::fs::write(output, png::encode(&rendered).unwrap()).unwrap();
    t.finish();
}

fn find_with_value<'a>(v: &'a Value, value: &str) -> Option<&'a str> {
    if v["kind"] == "text_input" && v["value"] == value {
        return v["id"].as_str();
    }
    v["children"]
        .as_array()?
        .iter()
        .find_map(|v| find_with_value(v, value))
}

#[test]
fn window_feedback_spec() {
    let t = Test::new();
    let dest = t.fixture("new.reelio");
    t.dialog(&[&dest]);
    let rev = t.host.scene_revision(MAIN);
    t.command("project.save");
    t.wait(MAIN, rev, |s| {
        s.by_text("Project saved: new.reelio").is_some()
    })
    .assert_node("toast.overlay");
    let press = pointer(PointerPhase::Enter, 4., 4., 0);
    t.host.event(MAIN, "toast:notice:hover", press).unwrap();
    std::thread::sleep(Duration::from_millis(5150));
    assert!(SceneSnapshot {
        revision: 0,
        root: t.host.scene(MAIN).unwrap()
    }
    .node("toast.overlay")
    .is_some());
    t.click(MAIN, "toast:notice:dismiss");
    let rev = t.change(MAIN, "viewer.name", json!("Unsaved"));
    t.wait(MAIN, rev, |s| find_with_value(&s.root, "Unsaved").is_some());
    t.command("project.quit");
    let id = t.modal("quit");
    t.wait(&id, 0, |s| s.node("quit:cancel").is_some());
    t.click(&id, "quit:cancel");
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&dest).unwrap()).unwrap()["name"],
        "Untitled project"
    );
    t.command("project.quit");
    let until = Instant::now() + TIMEOUT;
    while t.host.services(|s| {
        s.modals
            .iter()
            .filter(|v| v["contribution"] == "quit")
            .count()
    }) < 2
    {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(20));
    }
    let id = t.modal("quit");
    t.wait(&id, 0, |s| s.node("quit:confirm").is_some());
    let rev = t.host.scene_revision(MAIN);
    t.click(&id, "quit:confirm");
    t.wait(MAIN, rev, |s| s.node("workspace:reopen").is_some());
    t.finish();
}

#[test]
fn workspace_spec_import_trim_undo_crop_and_preview_resources() {
    let t = Test::new();
    let movie = t.media();
    let lane = t.import_video(&movie);
    let rev = t
        .host
        .event(
            MAIN,
            "divider:pool",
            pointer(PointerPhase::Down, 2., 12., 0),
        )
        .unwrap();
    t.host
        .event(MAIN, "divider:pool", pointer(PointerPhase::Up, 72., 12., 0))
        .unwrap();
    t.wait(MAIN, rev, |s| {
        s.assert_node("pool")["width"]["px"].as_u64().or_else(|| {
            s.assert_node("pool")["width"]["px"]
                .as_f64()
                .map(|v| v as u64)
        }) == Some(350)
    });
    assert_eq!(
        t.host
            .services(|s| s.settings["reelio.media-pool-width.v1"].clone()),
        "350"
    );
    t.host.show(SUMMARY).unwrap();
    t.wait(SUMMARY, 0, |s| s.node("cover:preview").is_some());
    let scene = SceneSnapshot {
        revision: 0,
        root: t.host.render_snapshot(MAIN).unwrap().0,
    };
    let ops = scene.assert_node(&lane)["draw"]["ops"].as_array().unwrap();
    let bounds = ops
        .iter()
        .find(|o| o["op"] == "rect" && o["stroke"] == "#6398ed")
        .or_else(|| {
            ops.iter()
                .filter(|o| o["op"] == "rect")
                .filter(|o| o["y"].as_f64().is_some_and(|y| y > 14.))
                .max_by(|a, b| {
                    a["w"]
                        .as_f64()
                        .unwrap_or(0.)
                        .total_cmp(&b["w"].as_f64().unwrap_or(0.))
                })
        })
        .expect("drawn clip");
    let right = bounds["x"].as_f64().unwrap() + bounds["w"].as_f64().unwrap() - 2.;
    let rev = t
        .host
        .event(MAIN, &lane, pointer(PointerPhase::Down, right, 40., 0))
        .unwrap();
    t.host
        .event(
            MAIN,
            &lane,
            pointer(PointerPhase::Move, right - 100., 40., 0),
        )
        .unwrap();
    t.host
        .event(MAIN, &lane, pointer(PointerPhase::Up, right - 100., 40., 0))
        .unwrap();
    t.wait(MAIN, rev, |s| {
        s.assert_node("undo")["enabled"] == true
            && s.assert_node(&lane)["draw"]["ops"]
                .as_array()
                .is_some_and(|new| new != ops)
    });
    let rev = t.click(MAIN, "undo");
    t.wait(MAIN, rev, |s| s.assert_node("redo")["enabled"] == true);
    let image = t.fixture("cover.jpg");
    assert!(Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:size=320x240",
            "-frames:v",
            "1",
            "-threads",
            "1"
        ])
        .arg(&image)
        .status()
        .unwrap()
        .success());
    let rev = t
        .host
        .event(
            SUMMARY,
            "cover:preview",
            NodeEvent::Drop {
                paths: vec![image.to_string_lossy().into_owned()],
                assets: vec![],
                x: 10.,
                y: 10.,
            },
        )
        .unwrap();
    t.wait(SUMMARY, rev, |s| {
        s.assert_node("cover:preview")["resource"] == "cover:project"
    });
    let rev = t
        .host
        .event(MAIN, "cover:lane:thumbnail", NodeEvent::DoubleClick)
        .unwrap();
    t.wait(MAIN, rev, |s| s.node("art:canvas").is_some());
    let rev = t.click(MAIN, "art:preset:5");
    t.wait(MAIN, rev, |s| {
        s.assert_node("art:crop_w")["value"] == 318 && s.assert_node("art:crop_h")["value"] == 212
    });
    t.host
        .wait_until(
            TIMEOUT,
            |st| {
                st.resources
                    .get("cover:preview")
                    .filter(|r| r["w"] == 318 && r["h"] == 212)
                    .map(|_| ())
            },
            "edited cover resource",
        )
        .expect("edited cover resource did not arrive");
    t.finish();
}
