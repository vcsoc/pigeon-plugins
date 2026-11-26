//! Plugin runtime on the Pigeon SDK (`pigeon-plugin-sdk`, vendored under `vendor/`).
//!
//! The SDK's protocol threads answer `ping`, write scenes and deliver typed events. This module
//! owns the Reelio state and turns SDK events into reducer events. Engine calls run on SDK worker
//! threads (`Ctx::spawn`), so the handler never waits on media work. State lives behind one mutex;
//! engine replies, the 100 ms tick and host events all update it through [`update`].

use crate::engine_bridge::EngineBridge;
use crate::player::{Cmd, Player};
use pigeon_plugin_sdk::canvas::DrawOp;
use pigeon_plugin_sdk::protocol::{Incoming, NodeEvent, PointerPhase};
use pigeon_plugin_sdk::runtime::{Ctx, PluginSpec};
use pigeon_plugin_sdk::{Contribution, DrawList, Node};
use reelio_ui::scene;
use reelio_ui::state::{Effect, Event, State};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// The embedded manifest is the single source of truth for the static contributions.
pub const MANIFEST: &str = include_str!("../../../plugin.json");

/// Plugin spec (id, version and static contributions from `plugin.json`).
pub fn spec() -> Result<PluginSpec, String> {
    let manifest: Value = serde_json::from_str(MANIFEST).map_err(|e| e.to_string())?;
    let mut spec = PluginSpec::new(
        manifest["id"].as_str().unwrap_or("reelio"),
        manifest["version"].as_str().unwrap_or("0.0.0"),
    );
    let items = manifest["ui"]["contributions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for item in items {
        let c: Contribution =
            serde_json::from_value(item).map_err(|e| format!("bad contribution: {e}"))?;
        spec = spec.contribution(c);
    }
    Ok(spec)
}

/// Split a scene into its structure and the canvas draw lists, keyed by node id. Structure-only
/// equality lets the pump send cheap canvas draws instead of whole scenes.
pub fn split_canvas(scene: &Value) -> (Value, Vec<(String, Value)>) {
    let mut draws = vec![];
    let mut stripped = scene.clone();
    strip(&mut stripped, &mut draws);
    (stripped, draws)
}

fn strip(v: &mut Value, draws: &mut Vec<(String, Value)>) {
    if v.get("kind").and_then(Value::as_str) == Some("canvas") {
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let draw = v.get("draw").cloned().unwrap_or(Value::Null);
        if let Some(obj) = v.as_object_mut() {
            obj.insert("draw".into(), Value::Null);
        }
        draws.push((id, draw));
    }
    match v {
        Value::Object(map) => map.values_mut().for_each(|value| strip(value, draws)),
        Value::Array(items) => items.iter_mut().for_each(|item| strip(item, draws)),
        _ => {}
    }
}

/// Controlled inputs carry a `rev` (amendment 7): it rises whenever the plugin changes the value,
/// so the host adopts the new text and ignores stale edits.
const CONTROLLED: [&str; 7] = [
    "text_input",
    "number_input",
    "slider",
    "select",
    "checkbox",
    "switch",
    "range_slider",
];

fn stamp_revs(v: &mut Value, instance: &str, revs: &mut BTreeMap<(String, String), (Value, u64)>) {
    if let Value::Object(map) = v {
        let kind = map
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if CONTROLLED.contains(&kind.as_str()) {
            if let (Some(id), Some(value)) = (
                map.get("id").and_then(Value::as_str).map(str::to_string),
                map.get("value").cloned(),
            ) {
                let key = (instance.to_string(), id);
                let rev = match revs.get(&key) {
                    Some((old, rev)) if *old == value => *rev,
                    Some((_, rev)) => rev + 1,
                    None => 1,
                };
                revs.insert(key, (value, rev));
                map.insert("rev".into(), json!(rev));
            }
        }
        for value in map.values_mut() {
            stamp_revs(value, instance, revs);
        }
    } else if let Value::Array(items) = v {
        for item in items {
            stamp_revs(item, instance, revs);
        }
    }
}

/// What the host last received, per instance.
#[derive(Default)]
pub struct Pump {
    sent_structure: BTreeMap<String, Value>,
    sent_draws: BTreeMap<(String, String), Value>,
    revs: BTreeMap<(String, String), (Value, u64)>,
    visible: BTreeMap<String, String>,
    open_modals: BTreeMap<String, Option<String>>,
    frame: u64,
    last_cue_reveal: Option<(String, u64)>,
}

impl Pump {
    /// Push whatever changed to the host through the SDK.
    pub fn render(&mut self, app: &Arc<App>, state: &State) {
        let ctx = &app.ctx;
        let scenes = scene::all(state);
        for name in scenes
            .keys()
            .filter(|k| k.starts_with("modal."))
            .cloned()
            .collect::<Vec<_>>()
        {
            if !self.open_modals.contains_key(&name) {
                self.open_modals.insert(name.clone(), None);
                let short = name.trim_start_matches("modal.").to_owned();
                let preferred = format!("reelio/{short}");
                let weak = Arc::downgrade(app);
                ctx.call_async(
                    "window.open_modal",
                    json!({"contribution":short,"instance":preferred,"props":{}}),
                    move |ctx, result| {
                        let Some(app) = weak.upgrade() else {
                            return;
                        };
                        match result {
                            Ok(value) => {
                                let instance = value["instance"]
                                    .as_str()
                                    .map(qualified_instance)
                                    .unwrap_or(preferred);
                                let ctx = ctx.clone();
                                update(&app, move |core| {
                                    if let Some(wire) = core.pump.open_modals.get_mut(&name) {
                                        *wire = Some(instance);
                                    } else {
                                        core_close_modal(&ctx, &instance);
                                    }
                                });
                            }
                            Err(e) => {
                                ctx.log("warn", &format!("Could not open Reelio modal: {e}"));
                                update(&app, move |core| dismiss_modal(core, &name));
                            }
                        }
                    },
                );
            }
        }
        let gone: Vec<String> = self
            .open_modals
            .keys()
            .filter(|m| !scenes.contains_key(*m))
            .cloned()
            .collect();
        for name in gone {
            if let Some(Some(instance)) = self.open_modals.remove(&name) {
                self.sent_structure.remove(&instance);
                core_close_modal(ctx, &instance);
            }
        }
        for (name, mut scene) in scenes {
            let Some(instance) = self
                .visible
                .get(&name)
                .or_else(|| self.open_modals.get(&name).and_then(Option::as_ref))
                .cloned()
            else {
                continue;
            };
            stamp_revs(&mut scene, &instance, &mut self.revs);
            let (structure, draws) = split_canvas(&scene);
            if self.sent_structure.get(&instance) == Some(&structure) {
                for (node, draw) in draws {
                    let key = (instance.clone(), node.clone());
                    if self.sent_draws.get(&key) != Some(&draw) {
                        self.frame += 1;
                        if let Ok(ops) = serde_json::from_value::<Vec<DrawOp>>(
                            draw.get("ops").cloned().unwrap_or(json!([])),
                        ) {
                            let _ = ctx.canvas_draw(
                                instance.clone(),
                                node,
                                self.frame,
                                DrawList { clear: None, ops },
                            );
                        }
                        self.sent_draws.insert(key, draw);
                    }
                }
            } else {
                for (node, draw) in draws {
                    self.sent_draws.insert((instance.clone(), node), draw);
                }
                self.sent_structure.insert(instance.clone(), structure);
                match serde_json::from_value::<Node>(scene) {
                    Ok(node) => {
                        if let Err(e) = ctx.set_scene(instance.clone(), node) {
                            ctx.log("warn", &format!("scene {instance} rejected: {e}"));
                        }
                    }
                    Err(e) => ctx.log(
                        "error",
                        &format!("scene {instance} is not a valid node tree: {e}"),
                    ),
                }
            }
        }
        if let Some(instance) = self
            .visible
            .get("main")
            .filter(|_| state.section == reelio_ui::state::Section::Subtitles)
        {
            if let Some(reveal) = state
                .studio
                .as_ref()
                .and_then(|s| s.workflow.reveal_cue.as_ref())
            {
                if self.last_cue_reveal.as_ref() != Some(reveal) {
                    self.last_cue_reveal = Some(reveal.clone());
                    let _ = ctx.scroll_to(instance, format!("sub:select:{}", reveal.0), "nearest");
                }
            }
        }
    }
}

fn qualified_instance(instance: &str) -> String {
    if instance.starts_with("reelio/") {
        instance.into()
    } else {
        format!("reelio/{instance}")
    }
}
fn contribution_id(instance: &str) -> &str {
    instance.strip_prefix("reelio/").unwrap_or(instance)
}
fn core_close_modal(ctx: &Ctx, instance: &str) {
    ctx.call_async(
        "window.close_modal",
        json!({"instance":instance}),
        |_, _| {},
    );
}
fn dismiss_modal(core: &mut Core, name: &str) {
    match name {
        "modal.consent" => {
            core.state.dialogs.engine_consent = None;
            if let Some((_, decision)) = core.consent.take() {
                let _ = decision.try_send(false);
            }
        }
        "modal.settings" => core.state.dialogs.settings = false,
        "modal.dependencies" => core.state.dialogs.dependencies = false,
        "modal.about" => core.state.dialogs.about = false,
        "modal.quit" => core.state.dialogs.quit_confirm = false,
        "modal.export_review" => core.state.dialogs.export_review = None,
        "modal.restore" => core.state.dialogs.restore_confirm = None,
        "modal.download" => {
            if let Some(id) = core.state.resources.front().map(|r| r.id.clone()) {
                core.state
                    .ui_action(&format!("resource:dismiss:{id}"), Value::Null);
            }
        }
        _ => {}
    }
}

/// Shared plugin core: Reelio state plus the pump that mirrors it to the host.
pub struct Core {
    pub state: State,
    pump: Pump,
    started: Instant,
    consent: Option<(Instant, std::sync::mpsc::SyncSender<bool>)>,
    consent_id: u64,
    images: crate::images::Images,
}

/// Everything the SDK callbacks need. Engine and ctx are immutable; the core is locked briefly.
pub struct App {
    pub ctx: Ctx,
    pub engine: Arc<dyn EngineBridge>,
    core: Mutex<Core>,
    /// Frame pump; set once after creation (it holds a weak handle back to this app).
    pub player: OnceLock<Player>,
}

impl App {
    pub fn new(ctx: Ctx, engine: Arc<dyn EngineBridge>) -> Arc<App> {
        let images = crate::images::Images::new(ctx.clone());
        Arc::new(App {
            ctx,
            engine,
            core: Mutex::new(Core {
                state: State::new(),
                pump: Pump::default(),
                started: Instant::now(),
                consent: None,
                consent_id: 0,
                images,
            }),
            player: OnceLock::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Apply `f` to the core, render the result, then start any engine requests on worker threads.
pub fn update(app: &Arc<App>, f: impl FnOnce(&mut Core) + Send + 'static) {
    update_boxed(app, Box::new(f));
}

/// Non-generic core of [`update`]; keeps the worker closure type fixed (no polymorphic recursion).
fn update_boxed(app: &Arc<App>, f: Box<dyn FnOnce(&mut Core) + Send>) {
    let requests = {
        let mut core = app.lock();
        f(&mut core);
        if core
            .consent
            .as_ref()
            .is_some_and(|(since, _)| since.elapsed() >= Duration::from_secs(110))
        {
            core.state.dialogs.engine_consent = None;
            if let Some((_, decision)) = core.consent.take() {
                let _ = decision.try_send(false);
            }
        }
        let mut requests = vec![];
        loop {
            let effects = core.state.drain();
            if effects.is_empty() {
                break;
            }
            for effect in effects {
                match effect {
                    Effect::Engine { id, request } => requests.push((id, request)),
                    Effect::Persist { key, value } => {
                        app.ctx.call_async(
                            "settings.set",
                            json!({"key": key, "value": value}),
                            |_, _| {},
                        );
                    }
                    Effect::ConsentDecision(approved) => {
                        if let Some((_, decision)) = core.consent.take() {
                            let _ = decision.try_send(approved);
                        }
                    }
                    Effect::CloseWindow => core.state.close_workspace(),
                    Effect::Seek { ms, .. } => send_player(app, Cmd::Seek { ms }),
                    Effect::Transport {
                        playing,
                        from_ms,
                        speed,
                        volume,
                    } => send_player(
                        app,
                        Cmd::Transport {
                            playing,
                            from_ms,
                            speed,
                            volume,
                        },
                    ),
                    Effect::LoadPreview { url, width, height } => {
                        send_player(app, Cmd::Load { url, width, height })
                    }
                    Effect::HostToast { level, text } => {
                        let level = if level == "error" { "error" } else { "info" };
                        let _ = app.ctx.toast(level, &text);
                    }
                }
            }
        }
        let Core {
            state,
            pump,
            images,
            ..
        } = &mut *core;
        images.update(&app.ctx, state, pump.visible.contains_key("main"));
        pump.render(app, state);
        requests
    };
    for (id, request) in requests {
        let app = app.clone();
        app.ctx.clone().spawn(move |_| {
            let reply = app.engine.call(request);
            update(&app, move |core| {
                core.state.dispatch(Event::Engine { id, reply })
            });
        });
    }
}

pub fn show_consent(
    app: &Arc<App>,
    prompt: reelio_engine::ConfirmDialog,
    decision: std::sync::mpsc::SyncSender<bool>,
) {
    update(app, move |core| {
        if core.consent.is_some() {
            let _ = decision.try_send(false);
            return;
        }
        core.consent_id += 1;
        core.state.dialogs.engine_consent = Some((core.consent_id, prompt.title, prompt.message));
        core.consent = Some((Instant::now(), decision));
    });
}

fn send_player(app: &Arc<App>, cmd: Cmd) {
    if let Some(player) = app.player.get() {
        player.send(cmd);
    }
}

/// Create the playback pump for `app`. Its clock reports back through a weak handle (no cycle).
pub fn attach_player(app: &Arc<App>) {
    let weak = Arc::downgrade(app);
    let on_clock = Arc::new(move |ms: f64| {
        if let Some(app) = weak.upgrade() {
            update(&app, move |core| {
                core.state.dispatch(Event::PlaybackClock(ms))
            });
        }
    });
    let _ = app.player.set(Player::start(app.ctx.clone(), on_clock));
}

/// Forward engine notifications (`EngineEvent`) into the reducer.
pub fn forward_engine_events(
    app: &Arc<App>,
    events: std::sync::mpsc::Receiver<reelio_engine::EngineEvent>,
) {
    let weak = Arc::downgrade(app);
    thread::spawn(move || {
        for event in events {
            let Some(app) = weak.upgrade() else { break };
            if let Ok(value) = serde_json::to_value(&event) {
                update(&app, move |core| {
                    core.state.dispatch(Event::EngineEvent(value))
                });
            }
        }
    });
}

/// Convert an SDK node event into the reducer's `ui` event (`node`, `value`).
pub fn ui_value(event: &NodeEvent) -> Option<Value> {
    Some(match event {
        NodeEvent::Click => Value::Null,
        NodeEvent::DoubleClick => json!({"type": "double_click"}),
        NodeEvent::Context => json!({"type": "context"}),
        NodeEvent::ListWindow {
            first,
            last,
            overscan,
        } => json!({"type": "list.window", "first": first, "last": last, "overscan": overscan}),
        NodeEvent::Change { value, .. } | NodeEvent::Submit { value, .. } => value.clone(),
        NodeEvent::Hover { .. } => json!({"type":"enter"}),
        NodeEvent::Blur { value, .. } if value.is_null() => json!({"type":"blur"}),
        NodeEvent::Blur { value, .. } => value.clone(),
        NodeEvent::Pointer {
            phase: PointerPhase::Down,
            x,
            y,
            button: 2,
            ..
        } => {
            json!({"type": "context", "x": x, "y": y})
        }
        NodeEvent::Pointer {
            phase, x, y, mods, ..
        } => {
            let phase = match phase {
                PointerPhase::Down => "down",
                PointerPhase::Move => "move",
                PointerPhase::Up => "up",
                PointerPhase::Cancel => "cancel",
                PointerPhase::Enter => return Some(json!({"type":"enter"})),
                PointerPhase::Leave => return Some(json!({"type":"leave"})),
            };
            json!({"type": "pointer", "phase": phase, "x": x, "y": y, "mods": mods_json(mods)})
        }
        NodeEvent::Wheel {
            dx, dy, x, y, mods, ..
        } => {
            json!({"type": "wheel", "dx": dx, "dy": dy, "x": x, "y": y, "mods": mods_json(mods)})
        }
        NodeEvent::Key {
            phase, key, mods, ..
        } => {
            let phase = match phase {
                pigeon_plugin_sdk::protocol::KeyPhase::Down => "down",
                _ => "up",
            };
            json!({"type": "key", "phase": phase, "key": key, "mods": mods_json(mods)})
        }
        _ => return None,
    })
}

fn mods_json(mods: &pigeon_plugin_sdk::protocol::Mods) -> Value {
    let mut m = Map::new();
    m.insert("shift".into(), json!(mods.shift));
    m.insert("ctrl".into(), json!(mods.ctrl));
    m.insert("alt".into(), json!(mods.alt));
    m.insert("meta".into(), json!(mods.meta));
    Value::Object(m)
}

/// Handle one SDK message. Runs on the SDK app thread; never blocks on engine work.
pub fn handle(app: &Arc<App>, msg: Incoming) {
    match msg {
        Incoming::Visibility { instance, visible } => update(app, move |core| {
            let key = contribution_id(&instance).to_owned();
            if visible && matches!(key.as_str(), "main" | "summary" | "project" | "prefs") {
                core.pump.visible.insert(key, instance.clone());
                core.pump.sent_structure.remove(&instance);
            } else if !visible {
                core.pump.visible.remove(&key);
                if let Some(name) = core.pump.open_modals.iter().find_map(|(name, wire)| {
                    (wire.as_ref() == Some(&instance)).then(|| name.clone())
                }) {
                    dismiss_modal(core, &name);
                }
            }
        }),
        Incoming::UiEvent {
            node,
            event,
            instance,
            ..
        } => {
            if let NodeEvent::ListWindow {
                first,
                last,
                overscan,
            } = event
            {
                if node == "sub:list" && contribution_id(&instance) == "main" {
                    let core = app.lock();
                    let (start, rows) = reelio_ui::scene::cue_rows(
                        &core.state,
                        first.saturating_sub(overscan),
                        last.saturating_add(overscan),
                    );
                    let rows = rows
                        .into_iter()
                        .filter_map(|row| serde_json::from_value(row).ok())
                        .collect();
                    let _ = app.ctx.list_rows(instance, node, start, rows);
                }
                return;
            }
            let value = if node == "viewer.seek" {
                match &event {
                    NodeEvent::Submit { value, .. } | NodeEvent::Blur { value, .. }
                        if value.is_number() =>
                    {
                        Some(json!({"ms":value,"final":true}))
                    }
                    _ => ui_value(&event),
                }
            } else {
                ui_value(&event)
            };
            if let Some(value) = value {
                update(app, move |core| {
                    core.state.dispatch(Event::Ui { node, value })
                });
            } else if let NodeEvent::Drop { paths, .. } = event {
                let drop = if node == "cover:preview" {
                    Event::CoverDrop { paths }
                } else {
                    Event::Drop { paths }
                };
                update(app, move |core| core.state.dispatch(drop));
            }
        }
        Incoming::Command { command, .. } => update(app, move |core| {
            core.state.dispatch(Event::Command { id: command })
        }),
        Incoming::Drop { paths, target, .. } => {
            let drop = if target == "cover-drop" {
                Event::CoverDrop { paths }
            } else {
                Event::Drop { paths }
            };
            update(app, move |core| core.state.dispatch(drop));
        }
        Incoming::Resize { w, h, instance, .. } if contribution_id(&instance) == "main" => {
            update(app, move |core| {
                core.state.set_window(w as f64);
                core.state.window_height = f64::from(h).max(1.);
            })
        }
        Incoming::Focus { has: false } => update(app, |core| {
            for t in [&mut core.state.notice, &mut core.state.error]
                .into_iter()
                .flatten()
            {
                t.focused = false;
                t.hovering = false;
                t.paused = false;
            }
        }),
        _ => {}
    }
}

pub fn load_layout(app: &Arc<App>) {
    use reelio_ui::layout::*;
    for key in [
        WORKSPACE_LAYOUT_KEY,
        POOL_WIDTH_KEY,
        INSPECTOR_WIDTH_KEY,
        VIEWER_HEIGHT_KEY,
    ] {
        let weak = Arc::downgrade(app);
        app.ctx
            .call_async("settings.get", json!({"key":key}), move |_, result| {
                if let (Some(app), Ok(value)) = (weak.upgrade(), result) {
                    if !value["value"].is_null() {
                        update(&app, move |core| {
                            core.state.restore_layout_setting(key, &value["value"])
                        });
                    }
                }
            });
    }
}

/// Periodic clock for toasts, polling and debounced validation.
pub fn start_ticker(app: &Arc<App>) {
    let app = app.clone();
    let ctx = app.ctx.clone();
    ctx.spawn(move |_| loop {
        std::thread::sleep(Duration::from_millis(100));
        if !app.ctx.is_open() {
            break;
        }
        let now = app.lock().started.elapsed().as_secs_f64() * 1000.0;
        update(&app, move |core| core.state.dispatch(Event::Tick { now }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_contributions_parse_into_sdk_contributions() {
        let spec = spec().expect("manifest parses");
        assert_eq!(spec.id, "reelio");
        assert!(spec
            .contributions
            .iter()
            .any(|c| c.slot == "workspace.view" && c.id == "main"));
        assert_eq!(
            spec.contributions
                .iter()
                .filter(|c| c.slot == "inspector.panel")
                .count(),
            2
        );
    }

    #[test]
    fn every_lane_canvas_op_is_a_valid_sdk_draw_op() {
        // Reducer draw ops must deserialize into the SDK's typed DrawOp (amendment 3).
        use reelio_ui::canvas::{draw, lane_ops, LaneDraw};
        let lane = LaneDraw {
            width: 700.0,
            height: 56.0,
            kind: "video".into(),
            enabled: true,
            selected: true,
            title: "Movie".into(),
            start_label: "00:00:00.000".into(),
            end_label: "00:00:10.000".into(),
            ruler_step_ms: 1000.0,
            scale: 0.1,
            scroll: 0.0,
            clip_left: 10.0,
            clip_width: 400.0,
            trim_handles: true,
            ticks: vec![(0.0, "00:00:00.000".into())],
            chapters: vec![40.0],
            playhead_x: 120.0,
            excluded_pill: None,
        };
        let v = draw(1, lane_ops(&lane));
        serde_json::from_value::<Vec<DrawOp>>(v["ops"].clone())
            .expect("ops match the SDK draw schema");
    }

    #[test]
    fn controlled_inputs_bump_rev_only_when_the_value_changes() {
        let mut revs = BTreeMap::new();
        let mut a = json!({"id": "n", "kind": "text_input", "value": "x"});
        stamp_revs(&mut a, "main", &mut revs);
        assert_eq!(a["rev"], 1);
        let mut b = json!({"id": "n", "kind": "text_input", "value": "x"});
        stamp_revs(&mut b, "main", &mut revs);
        assert_eq!(b["rev"], 1);
        let mut c = json!({"id": "n", "kind": "text_input", "value": "y"});
        stamp_revs(&mut c, "main", &mut revs);
        assert_eq!(c["rev"], 2);
    }

    #[test]
    fn split_canvas_moves_draw_lists_out_of_the_structure() {
        let scene = json!({"id":"root","kind":"column","children":[{"id":"c","kind":"canvas","draw":{"ops":[{"op":"rect"}]}}]});
        let (structure, draws) = split_canvas(&scene);
        assert_eq!(draws.len(), 1);
        assert_eq!(draws[0].0, "c");
        assert!(structure["children"][0]["draw"].is_null());
    }
}
