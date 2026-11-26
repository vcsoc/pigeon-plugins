//! `pigeon-mock`: drive plugins the way Pigeon does, headless.
//!
//!   pigeon-mock validate-fixtures <examples-dir>
//!   pigeon-mock run <plugin-dir> [--show <instance>]
//!   pigeon-mock conformance <plugin-dir> [--cases DIR] [--goldens DIR] [--only ID] [--update-goldens] [--json FILE]
//!   pigeon-mock render <plugin-dir> --instance <id> --size WxH -o <png> [--theme dark|light]
//!   pigeon-mock render-scene <scene.json> --out <png> [--size WxH] [--theme dark|light] [--scale N]

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use pigeon_plugin_sdk::protocol::Incoming;
use pigeon_mock_host::conformance::{self, RunOptions};
use pigeon_mock_host::render::{self, Theme};
use pigeon_mock_host::{LaunchOptions, Schemas, Session};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("validate-fixtures") => validate_fixtures(args.get(1).map(PathBuf::from)),
        Some("conformance") => conformance_cmd(&args[1..]),
        Some("render") => match render_plugin(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => { eprintln!("{e}"); ExitCode::FAILURE }
        },
        Some("render-scene") => render_scene_cmd(&args[1..]),
        _ => {
            eprintln!("usage: pigeon-mock run|conformance|render|render-scene|validate-fixtures ... (see docs/architecture/sdk.md)");
            ExitCode::from(2)
        }
    }
}

/// Value of `--flag value`, if present.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].as_str())
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn run(args: &[String]) -> ExitCode {
    let Some(dir) = args.first() else {
        eprintln!("usage: pigeon-mock run <plugin-dir> [--show <instance>]");
        return ExitCode::from(2);
    };
    let schemas = Arc::new(Schemas::load());
    let session = match Session::launch(&PathBuf::from(dir), Arc::clone(&schemas), LaunchOptions::default()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("launch failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ready = match session.wait_ready(Duration::from_secs(10)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    println!("ready: {}", ready["plugin"]);
    if let Some(instance) = flag(args, "--show") {
        if let Err(e) = session.show(instance) {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
        match session.wait_until(Duration::from_secs(5), |st| st.scenes.get(instance).cloned(), "scene") {
            Some(scene) => println!("scene {instance}: {} nodes", scene_count(&scene)),
            None => {
                eprintln!("no scene for {instance} within 5 s");
                return ExitCode::FAILURE;
            }
        }
    }
    let exited = session.shutdown(Duration::from_secs(5));
    let violations = session.violations();
    for (v, line) in &violations {
        eprintln!("violation: {v} in {line}");
    }
    println!("shutdown clean: {exited}; violations: {}", violations.len());
    if violations.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn scene_count(v: &serde_json::Value) -> usize {
    1 + v["children"].as_array().map(|c| c.iter().map(scene_count).sum::<usize>()).unwrap_or(0)
}

fn conformance_cmd(args: &[String]) -> ExitCode {
    let Some(plugin) = args.first() else {
        eprintln!("usage: pigeon-mock conformance <plugin-dir> [--cases DIR] [--goldens DIR] [--only ID] [--update-goldens] [--json FILE]");
        return ExitCode::from(2);
    };
    let root = repo_root();
    let cases_dir = flag(args, "--cases").map(PathBuf::from).unwrap_or_else(|| root.join("conformance/cases"));
    let goldens = flag(args, "--goldens").map(PathBuf::from).unwrap_or_else(|| root.join("conformance/goldens"));
    let only = flag(args, "--only");
    if has(args, "--update-goldens") {
        // SAFETY-free: environment is read by the golden comparison in this process only.
        std::env::set_var("UPDATE_GOLDENS", "1");
    }
    let cases = match conformance::load_cases(&cases_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let schemas = Arc::new(Schemas::load());
    let opts = RunOptions { goldens, timeout: Duration::from_secs(5) };
    let mut results = Vec::new();
    for (_, case) in cases {
        if let Some(id) = only {
            if case["id"].as_str() != Some(id) {
                continue;
            }
        }
        let r = conformance::run_case(&case, &PathBuf::from(plugin), Arc::clone(&schemas), &opts);
        let mark = if r.passed { "PASS" } else { "FAIL" };
        println!("{mark} {} ({:.1}s){}", r.id, r.elapsed.as_secs_f64(), if r.skipped > 0 { format!(" [{} skipped]", r.skipped) } else { String::new() });
        for (n, name, outcome) in &r.steps {
            match outcome {
                conformance::Outcome::Fail(m) => println!("    step {n} {name}: {m}"),
                conformance::Outcome::Skipped(m) => println!("    step {n} {name}: skipped: {m}"),
                conformance::Outcome::Pass => {}
            }
        }
        results.push(r);
    }
    let failed = results.iter().filter(|r| !r.passed).count();
    println!("{} case(s), {} failed", results.len(), failed);
    if let Some(path) = flag(args, "--json") {
        let report: Vec<serde_json::Value> = results
            .iter()
            .map(|r| serde_json::json!({"id": r.id, "title": r.title, "passed": r.passed, "skipped": r.skipped,
                "steps": r.steps.iter().map(|(n, name, o)| serde_json::json!({"step": n, "name": name, "outcome": format!("{o:?}")})).collect::<Vec<_>>()}))
            .collect();
        if let Err(e) = std::fs::write(path, serde_json::to_string_pretty(&report).unwrap_or_default()) {
            eprintln!("cannot write {path}: {e}");
        }
    }
    if failed == 0 && !results.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// The repository root (two levels above the crate's `sdk/rust` workspace).
fn repo_root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.join("../../..").canonicalize().unwrap_or_else(|_| PathBuf::from("."))
}

fn render_plugin(args: &[String]) -> Result<(), String> {
    let usage = "usage: pigeon-mock render <plugin-dir> --instance <id> --size WxH -o <png> [--theme dark|light] [--scale N]";
    let dir = Path::new(args.first().ok_or(usage)?);
    let id = flag(args, "--instance").ok_or(usage)?;
    let out = flag(args, "-o").or_else(|| flag(args, "--out")).ok_or(usage)?;
    let (w, h) = flag(args, "--size").ok_or(usage)?.split_once(['x', 'X']).ok_or("size must be WxH")?;
    let (w, h): (u32, u32) = (w.parse().map_err(|_| "invalid width")?, h.parse().map_err(|_| "invalid height")?);
    let scale: f32 = flag(args, "--scale").unwrap_or("1").parse().map_err(|_| "invalid scale")?;
    let (pw, ph) = (w as f32 * scale, h as f32 * scale);
    if !scale.is_finite() || pw < 1.0 || ph < 1.0 || pw > 8192.0 || ph > 8192.0 || pw * ph > 16_777_216.0 {
        return Err("render size is outside supported bounds (8192 per side, 16M pixels total)".into());
    }
    let light = match flag(args, "--theme").unwrap_or("dark") {
        "dark" => false, "light" => true, _ => return Err("theme must be dark or light".into()),
    };
    let schemas = Arc::new(Schemas::load());
    let manifest = pigeon_mock_host::session::load_manifest(dir, &schemas)?;
    let mut theme = render::theme::protocol_theme(light, scale);
    if let Some(overrides) = manifest.ui["theme_override"].as_object() {
        for (token, color) in overrides {
            if let Some(hex) = color.as_str().filter(|s| render::Rgba::parse_hex(s).is_some()) {
                theme.colors.insert(token.clone(), hex.to_string());
            }
        }
    }
    let palette = Theme::from_protocol(&theme);
    let launch = LaunchOptions { theme, scale, ..LaunchOptions::default() };
    let session = Session::launch(dir, schemas, launch).map_err(|e| e.to_string())?;
    session.wait_ready(Duration::from_secs(10))?;
    let instance = if id.contains('/') { id.to_owned() } else { format!("{}/{id}", manifest.id) };
    session.send(&Incoming::Resize { instance: instance.clone(), w: w as f32, h: h as f32, scale })?;
    session.show(&instance)?;
    session.wait_next_scene(&instance, 0, Duration::from_secs(5))?;
    let (scene, images) = session.render_snapshot(&instance).ok_or("scene disappeared")?;
    let surface = render::render_scene(&scene, (w, h), &palette, &images, scale);
    let clean = session.shutdown(Duration::from_secs(5));
    if let Some((violation, line)) = session.violations().first() { return Err(format!("{violation}: {line}")); }
    if !clean { return Err("plugin did not exit cleanly".into()); }
    let png = render::png::encode(&surface)?;
    std::fs::OpenOptions::new().write(true).create_new(true).open(out)
        .and_then(|mut file| file.write_all(&png)).map_err(|e| format!("{out}: {e}"))?;
    println!("wrote {out} ({}x{} px)", surface.w, surface.h);
    Ok(())
}

fn render_scene_cmd(args: &[String]) -> ExitCode {
    let (Some(input), Some(out)) = (args.first(), flag(args, "--out")) else {
        eprintln!("usage: pigeon-mock render-scene <scene.json> --out <png> [--size WxH] [--theme dark|light] [--scale N]");
        return ExitCode::from(2);
    };
    let size = flag(args, "--size").and_then(|s| {
        let (w, h) = s.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    });
    let (w, h) = size.unwrap_or((640, 480));
    let theme = if flag(args, "--theme") == Some("light") { Theme::light() } else { Theme::dark() };
    let scale: f32 = flag(args, "--scale").and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let text = match std::fs::read_to_string(input) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let scene: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let surface = render::render_scene(&scene, (w, h), &theme, &HashMap::new(), scale);
    match render::png::encode(&surface).and_then(|bytes| std::fs::write(out, bytes).map_err(|e| e.to_string())) {
        Ok(()) => {
            println!("wrote {out} ({}x{} px)", surface.w, surface.h);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn validate_fixtures(dir: Option<PathBuf>) -> ExitCode {
    let Some(dir) = dir else {
        eprintln!("usage: pigeon-mock validate-fixtures <schema/ui-v2/examples>");
        return ExitCode::from(2);
    };
    let schemas = Schemas::load();
    let mut failures = 0;
    let mut count = 0;
    for (sub, expect_valid) in [("valid", true), ("invalid", false)] {
        let mut entries: Vec<_> = std::fs::read_dir(dir.join(sub)).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let value: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    println!("{sub}/{name}: not JSON ({e})");
                    failures += 1;
                    continue;
                }
            };
            count += 1;
            let problems = if name.starts_with("manifest_ui") {
                schemas.check_manifest_ui(&value)
            } else if name.starts_with("to_host__") {
                schemas.check_to_host(&value)
            } else {
                schemas.check_to_plugin(&value)
            };
            if problems.is_empty() != expect_valid {
                failures += 1;
                println!("FAIL {sub}/{name}: {}", problems.first().map(|p| p.to_string()).unwrap_or_else(|| "accepted".into()));
            }
        }
    }
    println!("{count} fixtures checked, {failures} failure(s)");
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
