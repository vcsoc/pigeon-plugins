# Pigeon plugin UI protocol, version 2

Status: **normative contract** for the Pigeon 0.4 architecture. Every work package builds to this
document. Changes need the lead's approval (record them in the "Amendments" list at the bottom).

## 1. Principles

1. **Pigeon is a canvas of placeholders ("slots").** The host owns the window, the event loop,
   the theme, the library and a small set of host services. It contains **no plugin-specific UI
   or logic**: no Immich, no Reelio, no AI-editing panels, no semantic-search screens.
2. **A plugin contains everything it needs**: its UI description, logic, background workers,
   storage, dependencies and models. It is a separate OS process. Nothing is compiled into Pigeon.
3. **Plugins draw by description, not by linking.** A plugin sends a *scene* (a tree of widgets plus
   canvas draw lists) for each slot it occupies. The host renders it with its own toolkit (egui),
   forwards input back as events, and keeps the last scene on screen.
4. **The UI thread never waits for a plugin, a network, a disk or a database.** Ever. See section 9.
5. Enabling a plugin makes its slots appear; disabling hides them and stops the process;
   uninstalling deletes the plugin and its data. Pigeon's own library data is never touched by
   uninstalling a plugin.
6. Plugins stay independently versioned and fixable: a plugin bug is fixed by publishing a new
   plugin version, never by releasing Pigeon.

## 2. Process model and transport

* The host starts a plugin with `sh <plugin>/run --plugin` (cwd = plugin folder, environment as for
  protocol v1, plus `PIGEON_PLUGIN_DATA`, `PIGEON_SESSION_DIR`). Long-lived; one process per plugin.
* **Transport: UTF-8 JSON Lines on stdin (host to plugin) and stdout (plugin to host).** One JSON
  object per line, at most 1 MiB. stderr is diagnostics (the host keeps the last 200 KiB per plugin).
  stdout carries protocol only.
* **Bulk data never rides the pipe.** Pixels, video frames, thumbnails, files and large lists live in
  files under `PIGEON_SESSION_DIR` (a private temp folder the host creates per plugin session and
  deletes at exit) and are referenced by path (see "resources", section 6).
* One-shot headless operations from protocol v1 (`run` with JSON on stdin) remain supported for
  plugins that only provide operations; plugins with UI use `--plugin`.
* Launch policy (manifest `ui.activation`): `"startup"` (start when enabled), `"visible"` (start the
  first time one of its slots becomes visible or one of its actions is invoked; default),
  `"manual"`.

### Envelope

```json
{"v":2,"t":"<type>","id":"<optional correlation id>", ...payload}
```

`id` is set on requests; the matching response echoes it. Unknown `t` values are ignored (forward
compatibility). Both sides must tolerate unknown fields.

## 3. Handshake and capabilities

host to plugin, first line:

```json
{"v":2,"t":"hello","host":{"name":"Pigeon","version":"0.4.1"},"protocol":{"min":2,"max":2},
 "theme":{...tokens...},"locale":"en","scale":2.0,"data_dir":"...","session_dir":"...",
 "features":["scene.patch","canvas","list.window","resources.file","source","search"]}
```

plugin to host, within 10 s:

```json
{"v":2,"t":"ready","plugin":{"id":"reelio","version":"2.0.0"},"protocol":2,
 "contributions":[ ...slot contributions (section 4)... ]}
```

`contributions` may also be sent/updated later with `contribute` / `withdraw` (dynamic slots).
Static contributions are declared in `plugin.json` under `ui` so the host can show placeholders and
menus **before** the process starts (see section 11).

## 4. Slots (placeholders)

A *contribution* is `{"slot":"<slot id>","id":"<unique in plugin>", ...slot props}`. Each visible
contribution has a *slot instance* identified by `<plugin>/<id>` (plus `instance` for repeatable
ones such as modals). The plugin answers a `ui.visible` event for an instance by sending `ui.set`
with that instance's scene.

| Slot id | Where it appears | Props | Scene area |
|---|---|---|---|
| `sidebar.section` | a collapsible section in the left navigation | `title`, `icon`, `order`, `badge?` | width = sidebar, height auto (max `max_height`) |
| `sidebar.entry` | one row in the left navigation that opens a workspace view | `title`, `icon`, `order`, `badge?`, `opens` (a `workspace.view` id) | none (row only) |
| `toolbar.item` | a button/menu/group in the top toolbar | `group`, `order`, scene allowed (small) | height = toolbar |
| `workspace.view` | the whole central area, replacing the browse grid while active | `title`, `icon`, `toolbar?` | full central area, resizable |
| `inspector.panel` | a tab/section in the right inspector | `title`, `icon`, `order`, `applies_to` (media kinds/predicate) | inspector width |
| `preferences.page` | a page in Preferences | `title`, `icon`, `order` | page area |
| `modal` | a dialog opened by the plugin (host service `window.open_modal`) or by a contribution action | `title`, `size`, `resizable`, `modal:true|false` | dialog body |
| `statusbar.item` | the footer/status bar | `align`, `order`, scene allowed (small) | height = footer |
| `asset.action` | an entry in asset context menus / bulk actions / inspector action row | `title`, `icon`, `applies_to`, `selection:"one"|"many"|"any"`, `group`, `shortcut?` | none (a command) |
| `library.source` | a provider of assets shown in the browse grid and navigation tree (section 8) | `title`, `icon`, `root`, `capabilities` | provides items, not a scene |
| `search.provider` | an extra search mode in the search bar (section 8) | `title`, `icon`, `mode_id` | provides results, not a scene |
| `drop.target` | accepts files/assets dropped on a named region | `accepts`, `where` | none |
| `command` | a named command usable from menus, shortcuts and the command palette | `title`, `shortcut?`, `applies_to?` | none |

`applies_to` is `{"kinds":["image","video","audio","document"],"min":1,"max":null,"ext":["mkv"]}`.
The host evaluates it; the plugin is only asked to act when it matches.

Ordering: ascending `order`, ties by plugin id. The host may group by `group`. Users can hide any
contribution (preference); hidden contributions never start a plugin.

## 5. Scene

A scene is a tree. Every node has `id` (stable, unique within the slot instance; reuse across
updates preserves host-side state such as scroll position, caret and focus), `kind` and props.

```json
{"id":"root","kind":"column","gap":8,"padding":12,"children":[
  {"id":"title","kind":"text","style":"heading","text":"Reelio"},
  {"id":"go","kind":"button","label":"Open media","icon":"folder-open","variant":"primary","on":["click"]}
]}
```

### Layout kinds
`row`, `column`, `stack` (z-order overlay), `grid` (`columns` | `min_cell`), `wrap`, `split`
(`direction`, `children` exactly 2+, `sizes` fractions, `min`, user-resizable, `persist_key`),
`scroll` (`axis`, `show_bars`, scroll position synced via `scroll` event / `scroll_to` prop),
`tabs` (`tabs:[{id,label,icon}]`, `active`, content children selected by tab id), `collapsible`
(`title`, `open`), `panel` (framed, optional `title`, `fill`), `spacer`, `divider`.

Common props: `width`/`height` as `{"px":n}`, `{"fill":weight}` or `"auto"`; `min_*`/`max_*`;
`padding`, `margin`, `gap`; `align`/`justify`; `visible`; `enabled`; `tooltip`; `id_hint`;
`accessibility:{label,role}`; `bg`/`fg`/`border` as theme token names or `#rrggbb[aa]`;
`radius`.

### Content and controls
`text` (`style`: heading|title|body|caption|code|mono, `wrap`, `selectable`, `align`, `color`,
rich `spans:[{text,style,color}]`), `icon`, `image` (`resource`, `fit`: contain|cover|fill|none,
`on:["click","double_click","context"]`), `button` (`label`,`icon`,`variant`:
default|primary|danger|ghost|link, `toggled?`, `shortcut` display), `icon_button`, `checkbox`,
`switch`, `radio_group`, `segmented`, `text_input` (`value`, `rev`, `placeholder`, `multiline`,
`password`, `monospace`, `max_len`, `commit`: change|submit|blur, `completion?`), `number_input`
(`value`,`min`,`max`,`step`,`unit`,`precision`), `slider` (`value`,`min`,`max`,`step`,`marks?`),
`range_slider`, `select` (`options`,`value`,`searchable`), `color_picker`, `progress`
(`value|null`), `spinner`, `badge`/`pill`, `menu` (anchored popup: `items` tree with
`id,label,icon,shortcut,enabled,checked,separator,children`), `context_menu` (attached to any node:
`on:["context"]` opens `menu`), `tooltip` (rich), `table` (`columns`, `rows` window, selection,
sort), `tree` (`nodes`, expand state), `list` (virtualized, section 5.2), `canvas` (section 7),
`rich_text` (read-only, links), `code`.

### 5.1 Controlled inputs
Inputs have a plugin-owned value and a revision `rev`. The host keeps its own edit buffer while the
user types and sends `change` events carrying the buffer and the `rev` it was based on. The plugin
accepts it by sending the next scene with the same or higher `rev`; if the plugin sends a different
value with a **higher** `rev`, the host adopts it. Lower `rev` is ignored (stale). This prevents
the plugin from fighting the cursor.

### 5.2 Virtualized lists and tables
`list`: `{"kind":"list","count":N,"row_height":24|"variable","key":"rows","window":{"start":0,"rows":[...]}}`.
The host reports the visible range (`list.window` event with `first`,`last`, `overscan`); the
plugin answers `list.rows` with `{start,rows:[node...]}`; until then the host draws placeholders.
The scene itself never contains more than the window.

### 5.3 Updates
* `ui.set {instance, scene}`: replace the whole scene (diffed by id on the host).
* `ui.patch {instance, ops:[{op:"set"|"insert"|"remove"|"move", path, ...}]}` (feature
  `scene.patch`): ids as paths. Optional; `ui.set` is always valid.
* `ui.focus {instance, node}`, `ui.scroll_to {instance, node, align}`, `ui.beep`.
* Updates are applied atomically on the next frame. The host never shows a half-applied scene.

## 6. Resources (images, frames, icons)

```json
{"t":"resource.put","rid":"thumb:42","kind":"rgba|png|jpeg|webp|svg","file":"<abs path in session_dir>",
 "w":640,"h":360,"stride":2560,"seq":7,"premultiplied":false}
{"t":"resource.drop","rid":"thumb:42"}
```

`rid` is plugin-scoped. Re-sending the same `rid` with a higher `seq` replaces the pixels in place
(video frames, live previews). The host copies/uploads asynchronously and may delete nothing in the
file; the plugin reuses file names in a ring (>= 3 slots) and must not rewrite a slot the host has
not acknowledged (`resource.ack {rid,seq}`). Icons: nodes reference built-in icon names
(a Phosphor/Lucide-style set the host ships; list in `icons.md` of the SDK) or `resource` svg.

## 7. Canvas (custom drawing and direct manipulation)

```json
{"id":"timeline","kind":"canvas","width":{"fill":1},"height":{"px":180},"cursor":"default",
 "interactive":{"pointer":true,"wheel":true,"keys":true,"drop":true,"focusable":true,
                 "coalesce":"move"},
 "draw":{"frame":12,"clear":"#1b1d22","ops":[ ... ]}}
```

Draw ops (logical px, origin top-left of the canvas): `rect {x,y,w,h,fill,stroke,stroke_w,radius}`,
`line`, `polyline {pts,stroke,stroke_w}`, `polygon`, `path {cmds:[M,L,Q,C,Z],fill,stroke}`,
`circle`, `ellipse`, `arc`, `text {x,y,text,size,weight,color,align,baseline,max_w,font:"ui|mono"}`,
`image {rid,src?,dst,tint?,opacity?}`, `clip {x,y,w,h}` / `unclip`, `translate`/`scale`/`save`/
`restore`, `shadow`, `gradient`. A canvas has at most 20,000 ops and 4 MiB of JSON; larger frames
are rejected with `error {code:"canvas_too_large"}`.

Fast path: `canvas.draw {instance, node, frame, ops, region?}` updates one canvas without
resending the scene (used for timelines, waveforms, video overlays, scrubbing).

Canvas events (`ui.event` with `type`): `pointer {phase: down|move|up|cancel|enter|leave, x, y,
button, buttons, mods, clicks, pointer_id}`, `wheel {dx,dy,mods,x,y}`, `key {phase: down|up, key,
code, text, mods, repeat}`, `drop {paths|assets, x, y}`, `hover`. The host coalesces `pointer move`
and `wheel` events to at most one per rendered frame per canvas (with the latest coordinates and the
count of merged events); `down`/`up`/`key` are never dropped or reordered. The host captures the
pointer for the whole drag.

## 8. Providers (the plugin supplies data, the host draws it)

### 8.1 `library.source`
Declares a *source* whose assets appear in Pigeon's navigation tree and browse grid like local
folders do. The host owns grid layout, selection, sorting UI and thumbnails cache policy;
the plugin owns where the data comes from. Host to plugin requests (all answered asynchronously;
the host shows placeholders while waiting):

* `source.describe {source}` to `{title, icon, root:{id,title,children:bool}, capabilities:[
  "browse","search","upload","download","metadata_write","tags","delete","albums","live" ]}`.
* `source.children {source, node, cursor}` to `{nodes:[{id,title,icon,children:bool,count?}], next?}`.
* `source.list {source, node, cursor, limit, sort, filter}` to `{items:[Item], next?, total?}`.
  `Item = {id, name, kind, size?, w?, h?, duration?, taken_at?, modified?, tags?, thumb?:{file|rid},
  favorite?, meta?:{...}}`.
* `source.thumbnail {source, id, size}` to `{file}` (a file in `session_dir`) or `{error}`.
* `source.open {source, id, purpose:"view|edit|export"}` to `{file}` (cached local copy) or
  `{stream_url?}`.
* `source.upload {source, node, files:[path], options}` and `source.download {source, ids, dest}`
  return a *job id*; progress is reported through `progress` (section 10). Never blocks.
* `source.write_meta`, `source.delete`, `source.search {source, query, cursor}` as capabilities.
* Plugin to host push: `source.changed {source, node?, kind:"invalidate|insert|remove", ids?}`.

### 8.2 `search.provider`
`search.query {mode_id, query, filters, cursor, limit}` to `{results:[{ref, score?, snippet?}], next?,
status?}` where `ref` is a library asset id or a `{source,id}` item. `search.status` push for
indexing progress.

## 9. Non-blocking guarantees (normative)

* All plugin I/O runs on host reader/writer threads. The UI thread only swaps in the most recently
  completed scene and pushes events into bounded queues. It performs **no** blocking call into a
  plugin, a plugin file, a network or a database in a frame.
* A plugin that is slow, hung or crashed leaves its **last scene visible**, dimmed, with a
  "Not responding" badge after 2 s without any message while an event is outstanding. The host
  pings every 1 s (`ping`/`pong`, answered by the plugin's I/O thread, not its worker threads).
  After 5 s silent the badge offers Restart. Pigeon itself stays fully responsive.
* Event queues are bounded (host to plugin 512 events); coalescible events are merged, others are
  rejected with a visible warning rather than blocking. Scene size limits: 50,000 nodes, 4 MiB per
  `ui.set`; violations are errors, the old scene stays.
* Host service requests from the plugin have a timeout (default 30 s) and never run on the UI
  thread. Long work is a *job* (section 10).
* Plugins must run their heavy work on their own threads/processes and keep their stdin-reading
  loop free. The SDK enforces this with a dedicated protocol thread.
* Plugin crash: the host captures stderr, shows a placeholder in each of its slots with
  "Restart"/"Disable"/"Show log", and restarts with backoff (max 3 automatic restarts per 5 min).
* A frame budget test is part of conformance: with a plugin stalled for 10 s the host must still
  render at least 20 frames/s and answer input.

## 10. Host services (plugin to host requests)

Request: `{"v":2,"t":"req","id":"7","method":"library.query","params":{...}}`;
response: `{"t":"res","id":"7","result":{...}}` or `{"t":"res","id":"7","error":{"code","message"}}`.
Every method needs a manifest `permissions` entry; otherwise `error.code = "permission_denied"`.

| Method | Permission | Notes |
|---|---|---|
| `selection.get`, `selection.subscribe` (push `selection.changed`) | `selection` | ids, kinds, paths (for local assets) |
| `library.query {filter, sort, cursor, limit}` / `library.get {ids}` | `library.read` | asset records (public fields only) |
| `library.import {paths, mode:"copy|reference|derive", derive_of?, collection?, tags?}` | `library.write` | derivative links to the original |
| `library.set_tags`, `library.set_meta` | `library.write` | |
| `library.reveal {id}` | `library.read` | select/scroll in the grid |
| `settings.get/set/delete` | none | plugin-private, namespaced, persisted by the host |
| `secrets.get/set/delete` | `secrets` | OS keychain, plugin-private |
| `dialog.open_file/save_file/pick_folder` | `dialogs` | native dialogs, run off the UI thread |
| `clipboard.read/write` | `clipboard` | text, files, image |
| `shell.open_url/open_path/reveal` | `shell` | URL scheme allow-list https/http/mailto |
| `notify.toast {level, text, action?}` | none | |
| `progress.start/update/end {job}` | none | shows in the footer jobs area, cancellable (`cancel` push) |
| `window.open_modal {contribution,instance,props}` / `window.close_modal` / `window.set_title` | `windows` | |
| `drag.begin {paths}` | `drag` | native drag-out |
| `theme.get`, `locale.get` | none | |
| `log {level,message}` | none | |
| `net` / `fs` | n/a | not mediated: plugins use the OS directly (trusted code) |

Push messages host to plugin: `selection.changed`, `theme.changed`, `locale.changed`,
`visibility {instance,visible}`, `focus {has}`, `resize {instance,w,h,scale}`, `shutdown`,
`cancel {job}`, `command {id,args,context}` (an `asset.action`/`toolbar.item`/shortcut fired),
`drop {target,paths|assets}`.

## 11. Manifest (`plugin.json`) additions

```json
"ui": {
  "protocol": 2,
  "activation": "visible",
  "contributions": [ {"slot":"sidebar.entry","id":"nav","title":"Reelio","icon":"film","opens":"main"},
                     {"slot":"workspace.view","id":"main","title":"Reelio"} ],
  "permissions": ["selection","library.read","library.write","dialogs","shell","clipboard"]
},
"worker": {"runtime":"native|python3","entry":"run","protocol":2,"standalone":true, ...}
```

* `plugin.json` also carries `kind`: `"app"` (UI plugin), `"source"`, `"search"`, `"ops"` (v1),
  or several via `"provides"`.
* Native plugins list platform artifacts: `"artifacts":[{"target":"aarch64-apple-darwin",
  "file":"bin/reelio","url":"...","bytes":n,"sha256":"..."}]`. `run --setup` downloads and verifies
  the matching one, or builds from the pinned source with `cargo` when no artifact matches
  (developer mode). Python plugins create their private venv as in the standalone layout.
* The host reads `ui.contributions` from the installed manifest to show placeholders and menu
  entries without starting the plugin. It must work with zero plugins installed.
* Uninstall: the host deletes the plugin folder, then `.data` after a confirmation; it calls
  `run --uninstall` (optional, 30 s) first so the plugin can remove external state (for example a
  keychain entry) and never touches library data.

## 12. Theme tokens

`theme` in `hello` / `theme.changed`: `{"mode":"dark|light","scale":1.0,"font":{"ui":"...","mono":"..."},
"colors":{"bg":...,"bg_panel":...,"bg_raised":...,"bg_input":...,"fg":...,"fg_weak":...,"accent":...,
"accent_fg":...,"danger":...,"warning":...,"success":...,"border":...,"hover":...,"selection":...,
"focus_ring":...},"radius":4,"spacing":4}`. Nodes use token names (`"fg_weak"`) or literal colors.
A plugin may ship its own palette for visual parity with a product identity (`ui.theme_override`
in the manifest or per-node colors); the host still owns fonts, text rendering, DPI and
accessibility.

## 13. Testing and conformance

* **Mock host** (SDK): a headless implementation of the host side: sends `hello`/events, records
  scenes, validates every message against the JSON Schemas (`schema/ui-v2/*.schema.json`), and can
  render scenes to PNG (software renderer) for golden tests.
* **Conformance suite** (`conformance/`): run against (a) any plugin through the mock host, and
  (b) the real Pigeon host through `pigeon --conformance` (a test mode that loads a fixture plugin
  and exercises slots, input, resources, stalls, crashes and limits).
* Golden image tests are advisory for visual parity; every WP also needs behavioural tests
  (event to state to scene assertions).

## 14. Compatibility and migration

* Protocol v1 one-shot workers keep working during the migration; the built-in editor AI panel,
  semantic search UI and Immich code are removed from the host and re-provided by plugins that use
  this protocol. After migration the host has no `Capability`/`Operation` enums for plugins.
* The host publishes `protocol.min/max`; plugins declare the versions they speak. A plugin whose
  range does not include the host's is shown disabled with an explanation.

## Amendments
(none yet)
