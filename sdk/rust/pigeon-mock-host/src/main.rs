//! `pigeon-mock`: drive plugins the way Pigeon does, headless.
//!
//!   pigeon-mock validate-fixtures <dir>         validate valid/ and invalid/ fixture folders
//!   pigeon-mock run <plugin-dir> [--show <instance>] [--wait-ms N]
//!                                               launch, print ready, optional show, shut down

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use pigeon_mock_host::{LaunchOptions, Schemas, Session};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("validate-fixtures") => validate_fixtures(args.get(1).map(PathBuf::from)),
        _ => {
            eprintln!("usage: pigeon-mock run <plugin-dir> [--show <instance>] | validate-fixtures <dir>");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> ExitCode {
    let Some(dir) = args.first() else {
        eprintln!("usage: pigeon-mock run <plugin-dir> [--show <instance>]");
        return ExitCode::from(2);
    };
    let show = args.windows(2).find(|w| w[0] == "--show").map(|w| w[1].clone());
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
    if let Some(instance) = show {
        if let Err(e) = session.show(&instance) {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
        match session.wait_until(Duration::from_secs(5), |st| st.scenes.get(&instance).cloned(), "scene") {
            Some(scene) => println!("scene {instance}: {}", serde_json::to_string(&scene).unwrap_or_default()),
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
            let ok = problems.is_empty() == expect_valid;
            if !ok {
                failures += 1;
                println!("FAIL {sub}/{name}: {}", problems.first().map(|p| p.to_string()).unwrap_or_else(|| "accepted".into()));
            }
        }
    }
    println!("{count} fixtures checked, {failures} failure(s)");
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
