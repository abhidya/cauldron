//! The cauldron desktop face, on the Craft framework.
//!
//! The window chrome, buttons, text fields, checkboxes, progress bar and spinner are Craft
//! components in the ArtCraft dark theme. The surface, cards and feeds are painted by this
//! app with the same theme's colours.

use craft::behavior::placement::Defaults;
use craft::egui::{self, Align2, Color32, FontFamily, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use craft::theme::{ModeChoice, Variant};
use craft::ui::buttons::Button;
use craft::ui::choice::Checkbox;
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _};
use craft::ui::display::{Progress, Spinner};
use craft::ui::runtime::env::{RootEnv, WindowKey};
use craft::ui::runtime::frame::FrameInput;
use craft::ui::shell::app::{AppSpec, ShellApp, run};
use craft::ui::shell::{AppShell, ScreenMode};
use craft::ui::text::TextField;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

// ArtCraft dark, standard contrast, comfortable density (craft-theme's token dump).
const BG: Color32 = Color32::from_rgb(0x42, 0x42, 0x42);
const PANEL: Color32 = Color32::from_rgb(0x53, 0x53, 0x53);
const SOUP: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1c);
const SUNKEN: Color32 = Color32::from_rgb(0x45, 0x45, 0x45);
const TEXT: Color32 = Color32::from_rgb(0xee, 0xee, 0xee);
const MUTED: Color32 = Color32::from_rgb(0xd4, 0xd4, 0xd4);
const DIVIDER: Color32 = Color32::from_rgb(0x3e, 0x3e, 0x3e);
const EDGE: Color32 = Color32::from_rgb(0x68, 0x68, 0x68);
const ACCENT: Color32 = Color32::from_rgb(0x37, 0x8e, 0xf0);
const ACCENT_STRONG: Color32 = Color32::from_rgb(0x1b, 0x77, 0xd7);
const SELECTED: Color32 = Color32::from_rgb(0x6b, 0x6b, 0x6b);
const GOOD_FG: Color32 = Color32::from_rgb(0x9d, 0xe7, 0x87);
const GOOD_BG: Color32 = Color32::from_rgb(0x12, 0x25, 0x0c);
const GOOD_EDGE: Color32 = Color32::from_rgb(0x22, 0x63, 0x00);
const WARN_FG: Color32 = Color32::from_rgb(0xfc, 0xcd, 0x78);
const WARN_BG: Color32 = Color32::from_rgb(0x29, 0x1d, 0x06);
const WARN_EDGE: Color32 = Color32::from_rgb(0x6c, 0x4c, 0x00);
const WARN: Color32 = Color32::from_rgb(0xe8, 0xb0, 0x46);


#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Welcome,
    Clone,
    Ingest,
    Main,
}
#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Cauldron,
    Threads,
    Settings,
}
#[derive(PartialEq, Clone, Copy)]
enum Sub {
    Surface,
    Review,
    Tasks,
}

enum Msg {
    CloneProgress(String),
    CloneDone(Result<String, String>),
    Dumped { index: usize, id: Result<String, String> },
    Imported { batch: usize, result: Result<String, String> },
    Search { thread: usize, item: usize, rows: Vec<[String; 3]>, total: u64 },
    Acts(String),
    Ran { item: usize, ok: bool, tail: Vec<String> },
}

struct Doc {
    path: String,
    reason: &'static str,
    ticked: bool,
}

struct Drop {
    at: f64,
    dump: String,
    pos: [f32; 2],
    label: String,
    reply: usize,
}

enum Item {
    User(String),
    Agent { who: String, adapter: String, text: String },
    Tool { who: String, adapter: String, query: String, rows: Option<(Vec<[String; 3]>, u64)> },
    Peer { from: String, to: String, text: String },
    Card(&'static str),
    Note(String),
    Run { cmd: String, out: Option<(bool, Vec<String>)> },
}

struct Thread {
    title: String,
    items: Vec<Item>,
    step: u32,
    step_at: f64,
    dumped: Option<String>,
    done: bool,
}

struct App {
    env: RootEnv,
    mode: ScreenMode,
    cauldron_bin: String,
    cauldron_env: String,
    demo_dir: PathBuf,
    hitmap: Option<PathBuf>,
    replay: Value,
    records: BTreeMap<String, Value>,
    screen: Screen,
    tab: Tab,
    sub: Sub,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    repo: String,
    dest: String,
    name: String,
    cloning: bool,
    clone_line: String,
    clone_error: Option<String>,
    setup_note: String,
    docs: Vec<Doc>,
    ingest_at: Option<f64>,
    drops: Vec<Drop>,
    dump_error: Option<String>,
    pending: Vec<String>,
    approved: BTreeSet<String>,
    flagged: bool,
    importing: bool,
    import_note: Option<(bool, String)>,
    board_open: bool,
    board_at: f64,
    reply: String,
    replied: Option<(String, f64)>,
    threads: Vec<Thread>,
    cur: usize,
    prompt: String,
    task_state: usize,
    acts: Option<String>,
    toast: Option<(String, f64)>,
    hits: BTreeMap<String, [f32; 2]>,
    flags: BTreeMap<String, bool>,
    last_written: String,
    tex: Option<egui::TextureHandle>,
    now: f64,
    pointer: bool,
    task_note: Option<String>,
    note: String,
}

fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Paints wrapped text at `pos` within `width`; returns its height.
fn text(ui: &Ui, pos: Pos2, width: f32, s: &str, f: FontId, col: Color32) -> f32 {
    let galley = ui.painter().layout(s.to_owned(), f, col, width.max(20.0));
    let h = galley.size().y;
    ui.painter().galley(pos, galley, col);
    h
}
fn line(ui: &Ui, pos: Pos2, s: &str, size: f32, col: Color32) {
    ui.painter().text(pos, Align2::LEFT_TOP, s, font(size), col);
}
fn measure(ui: &Ui, s: &str, f: FontId, width: f32) -> egui::Vec2 {
    ui.painter().layout(s.to_owned(), f, TEXT, width.max(20.0)).size()
}
fn fill(ui: &Ui, r: Rect, radius: f32, col: Color32) {
    ui.painter().rect_filled(r, radius, col);
}
fn outline(ui: &Ui, r: Rect, radius: f32, col: Color32, w: f32) {
    ui.painter().rect_stroke(r, radius, Stroke::new(w, col), StrokeKind::Inside);
}
/// Formerly a state pill; draws nothing.
fn tag(_ui: &Ui, _pos: Pos2, _real: bool) -> f32 {
    0.0
}
/// Runs `f` in a child ui laid out left to right inside `rect`; returns its result and the rect it used.
fn at<R>(ui: &mut Ui, rect: Rect, f: impl FnOnce(&mut Ui) -> R) -> (R, Rect) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let r = f(&mut child);
    (r, child.min_rect())
}

fn run_cli(bin: &str, args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut c = Command::new(bin);
    c.args(args).stdin(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = c.output().map_err(|e| format!("could not run {bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() { Ok(stdout) } else { Err(format!("{}{}", stdout, String::from_utf8_lossy(&out.stderr)).trim().to_string()) }
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let demo_dir = PathBuf::from(std::env::var("CAULDRON_DEMO_DIR").unwrap_or_else(|_| "demo".into()));
        let replay: Value = serde_json::from_str(&std::fs::read_to_string(demo_dir.join("replay.json")).expect("demo/replay.json")).expect("replay.json parses");
        let mut records = BTreeMap::new();
        for b in ["batch-1.json", "batch-2.json"] {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(demo_dir.join(b)).expect("batch file")).expect("batch parses");
            for r in v.as_array().into_iter().flatten() {
                records.insert(r["id"].as_str().unwrap_or_default().to_string(), r.clone());
            }
        }
        let tex = std::fs::read(demo_dir.join("cauldron.rgba")).ok().filter(|b| b.len() == 560 * 315 * 4).map(|b| {
            ctx.load_texture("illustration", egui::ColorImage::from_rgba_unmultiplied([560, 315], &b), egui::TextureOptions::LINEAR)
        });
        let (tx, rx) = channel();
        let mut env = RootEnv::default();
        env.mode = ModeChoice::Dark;
        let threads = replay["threads"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| Thread { title: t["title"].as_str().unwrap_or_default().into(), items: Vec::new(), step: 0, step_at: 0.0, dumped: None, done: false })
            .collect();
        App {
            env,
            mode: ScreenMode::default(),
            cauldron_bin: std::env::var("CAULDRON_BIN").unwrap_or_else(|_| "cauldron".into()),
            cauldron_env: std::env::var("CAULDRON_DEMO_ENV").unwrap_or_else(|_| "demo".into()),
            hitmap: std::env::var("CAULDRON_GUI_HITMAP").ok().map(PathBuf::from),
            demo_dir,
            replay,
            records,
            screen: Screen::Welcome,
            tab: Tab::Cauldron,
            sub: Sub::Surface,
            tx,
            rx,
            repo: String::new(),
            dest: std::env::var("CAULDRON_DEMO_DEST").unwrap_or_else(|_| "/tmp/projects/GotYaForce".into()),
            name: "gotyaforce".into(),
            cloning: false,
            clone_line: String::new(),
            clone_error: None,
            setup_note: String::new(),
            docs: Vec::new(),
            ingest_at: None,
            drops: Vec::new(),
            dump_error: None,
            pending: Vec::new(),
            approved: BTreeSet::new(),
            flagged: false,
            importing: false,
            import_note: None,
            board_open: false,
            board_at: 0.0,
            reply: String::new(),
            replied: None,
            threads,
            cur: 0,
            prompt: String::new(),
            task_state: 0,
            acts: None,
            toast: None,
            hits: BTreeMap::new(),
            flags: BTreeMap::new(),
            last_written: String::new(),
            tex,
            now: 0.0,
            pointer: std::env::var("CAULDRON_GUI_POINTER").is_ok(),
            task_note: None,
            note: String::new(),
        }
    }

    fn hit(&mut self, key: &str, r: Rect) {
        self.hits.insert(key.to_string(), [r.center().x, r.center().y]);
    }
    fn flag(&mut self, key: &str, v: bool) {
        self.flags.insert(key.to_string(), v);
    }

    /// A Craft button in `rect`, registered for the recording driver under `key`.
    fn button(&mut self, ui: &mut Ui, key: &str, rect: Rect, label: &str, primary: bool) -> bool {
        let label = label.to_string();
        let (clicked, used) = at(ui, rect, |ui| {
            let b = Button::new(label);
            let b = if primary { b.variant(Variant::PRIMARY) } else { b };
            b.show(ui).clicked()
        });
        self.hit(key, used);
        clicked
    }

    fn cluster_of(&self, record: &Value) -> String {
        let region = record["edges"].as_array().into_iter().flatten().find(|e| e["kind"] == "in_region").and_then(|e| e["to"].as_str()).unwrap_or_default();
        self.replay["clusters"].as_array().into_iter().flatten().find(|c| c["region"] == region).and_then(|c| c["name"].as_str()).unwrap_or_default().to_string()
    }

    fn start_clone(&mut self) {
        let repo = self.repo.trim().trim_start_matches("https://github.com/").trim_end_matches(".git").to_string();
        if repo.split('/').count() != 2 || repo.contains(' ') {
            self.clone_error = Some("Enter a repository as owner/name.".into());
            return;
        }
        self.clone_error = None;
        self.cloning = true;
        self.clone_line = "Starting git…".into();
        let (tx, dest, bin, env, name) = (self.tx.clone(), self.dest.clone(), self.cauldron_bin.clone(), self.cauldron_env.clone(), self.name.clone());
        std::thread::spawn(move || {
            let url = format!("https://github.com/{repo}");
            let child = Command::new("git").args(["clone", "--depth", "1", "--progress", &url, &dest]).env("GIT_LFS_SKIP_SMUDGE", "1").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Msg::CloneDone(Err(format!("git could not start: {e}"))));
                    return;
                }
            };
            let mut err = child.stderr.take().expect("stderr");
            let (mut buf, mut cur, mut all) = ([0u8; 256], String::new(), String::new());
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                for &b in &buf[..n] {
                    if b == b'\r' || b == b'\n' {
                        if !cur.trim().is_empty() {
                            let _ = tx.send(Msg::CloneProgress(cur.trim().to_string()));
                            all = cur.trim().to_string();
                        }
                        cur.clear();
                    } else {
                        cur.push(b as char);
                    }
                }
            }
            if !child.wait().map(|s| s.success()).unwrap_or(false) {
                let _ = tx.send(Msg::CloneDone(Err(format!("git clone failed: {all}"))));
                return;
            }
            let _ = tx.send(Msg::CloneProgress("Creating the cauldron…".into()));
            let made = run_cli(&bin, &["create", &name, "--env", &env], None).and_then(|_| run_cli(&bin, &["link", &name, "--env", &env], Some(Path::new(&dest))));
            let _ = tx.send(Msg::CloneDone(made.map(|_| format!("cauldron `{name}` created and linked to {dest}"))));
        });
    }

    fn scan_docs(&mut self) {
        let ticked: Vec<String> = self.replay["docs"].as_array().into_iter().flatten().filter_map(|d| d["path"].as_str().map(String::from)).collect();
        let mut found = Vec::new();
        for dir in ["", "docs"] {
            let Ok(rd) = std::fs::read_dir(Path::new(&self.dest).join(dir)) else { continue };
            let mut names: Vec<String> = rd.filter_map(|e| e.ok()).filter(|e| e.path().is_file()).filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".md")).collect();
            names.sort();
            for n in names {
                found.push(if dir.is_empty() { n } else { format!("{dir}/{n}") });
            }
        }
        self.docs = found
            .into_iter()
            .map(|path| {
                let low = path.to_lowercase();
                let reason = if low.ends_with("readme.md") {
                    "readme"
                } else if low.contains("agents") || low.contains("claude") {
                    "agent instructions"
                } else if ["status", "spec", "completion", "loop", "spike", "design"].iter().any(|k| low.contains(k)) {
                    "decision doc"
                } else {
                    "doc"
                };
                Doc { ticked: ticked.contains(&path), path, reason }
            })
            .collect();
    }

    fn start_ingest(&mut self) {
        self.ingest_at = Some(self.now);
        self.screen = Screen::Main;
        let docs: Vec<String> = self.docs.iter().filter(|d| d.ticked).map(|d| d.path.clone()).collect();
        let (tx, dest, bin, env, name) = (self.tx.clone(), self.dest.clone(), self.cauldron_bin.clone(), self.cauldron_env.clone(), self.name.clone());
        std::thread::spawn(move || {
            for (index, doc) in docs.iter().enumerate() {
                std::thread::sleep(std::time::Duration::from_millis(if index == 0 { 1800 } else { 2600 }));
                let file = Path::new(&dest).join(doc);
                let out = run_cli(&bin, &["soup", "dump", &file.to_string_lossy(), "--cauldron", &name, "--env", &env, "--title", doc, "--source", "ingest"], None);
                let id = out.and_then(|o| serde_json::from_str::<Value>(&o).ok().and_then(|v| v["dump_id"].as_str().map(String::from)).ok_or_else(|| "no dump id in the receipt".to_string()));
                let _ = tx.send(Msg::Dumped { index, id });
            }
        });
    }

    fn approve(&mut self) {
        let batch = if self.pending.iter().any(|p| p == "DEC-1") { 1 } else { 2 };
        self.importing = true;
        let file = self.demo_dir.join(format!("batch-{batch}.json"));
        let (tx, bin, env, name) = (self.tx.clone(), self.cauldron_bin.clone(), self.cauldron_env.clone(), self.name.clone());
        std::thread::spawn(move || {
            let out = run_cli(&bin, &["soup", "import", &file.to_string_lossy(), "--cauldron", &name, "--env", &env, "--agent", "ripple"], None);
            let result = out.and_then(|o| {
                let v: Value = serde_json::from_str(&o).map_err(|e| format!("unreadable answer: {e}"))?;
                if v["refused"].as_u64().unwrap_or(1) == 0 && v["dry_run"] != true { Ok(format!("cauldron soup import: {} · {} accepted, 0 refused", v["outcome"].as_str().unwrap_or("landed"), v["accepted"])) } else { Err(o) }
            });
            let _ = tx.send(Msg::Imported { batch, result });
        });
    }

    fn search(&mut self, thread: usize, item: usize, query: &str) {
        let (tx, bin, env, name, q) = (self.tx.clone(), self.cauldron_bin.clone(), self.cauldron_env.clone(), self.name.clone(), query.to_string());
        std::thread::spawn(move || {
            let out = run_cli(&bin, &["soup", "search", &q, "--cauldron", &name, "--env", &env, "--limit", "3", "--max-chars", "90"], None).unwrap_or_default();
            let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            let rows = v["results"].as_array().into_iter().flatten().map(|r| [r["id"].as_str().unwrap_or("").to_string(), r["type"].as_str().unwrap_or("").to_string(), r["handle"].as_str().unwrap_or("").to_string()]).collect();
            let _ = tx.send(Msg::Search { thread, item, rows, total: v["total"].as_u64().unwrap_or(0) });
        });
    }

    fn dump_session(&mut self) {
        let t = &self.threads[self.cur];
        let mut out = format!("# Thread: {}\n\n", t.title);
        for it in &t.items {
            match it {
                Item::User(s) => out += &format!("user: {s}\n\n"),
                Item::Agent { who, adapter, text } => out += &format!("{who} ({adapter}): {text}\n\n"),
                Item::Tool { who, query, rows, .. } => {
                    out += &format!("{who} tool call: soup search \"{query}\"\n");
                    for r in rows.iter().flat_map(|r| r.0.iter()) {
                        out += &format!("  {} {} {}\n", r[0], r[1], r[2]);
                    }
                    out += "\n";
                }
                Item::Peer { from, to, text } => out += &format!("{from} -> {to}: {text}\n\n"),
                Item::Card(c) => out += &format!("[card: {c}]\n\n"),
                Item::Note(s) => out += &format!("note: {s}\n\n"),
                Item::Run { cmd, out: o } => {
                    out += &format!("ran: {cmd}\n");
                    for l in o.iter().flat_map(|o| o.1.iter()) {
                        out += &format!("  {l}\n");
                    }
                    out += "\n";
                }
            }
        }
        let file = std::env::temp_dir().join("cauldron-gui-thread.md");
        let _ = std::fs::write(&file, out);
        let (tx, bin, env, name, title) = (self.tx.clone(), self.cauldron_bin.clone(), self.cauldron_env.clone(), self.name.clone(), format!("thread: {}", t.title));
        std::thread::spawn(move || {
            let out = run_cli(&bin, &["soup", "dump", &file.to_string_lossy(), "--cauldron", &name, "--env", &env, "--title", &title, "--source", "thread"], None);
            let id = out.and_then(|o| serde_json::from_str::<Value>(&o).ok().and_then(|v| v["dump_id"].as_str().map(String::from)).ok_or_else(|| "no dump id in the receipt".to_string()));
            let _ = tx.send(Msg::Dumped { index: 3, id });
        });
    }

    fn receive(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::CloneProgress(s) => self.clone_line = s,
                Msg::CloneDone(Ok(note)) => {
                    self.cloning = false;
                    self.setup_note = note;
                    self.scan_docs();
                    self.screen = Screen::Ingest;
                }
                Msg::CloneDone(Err(e)) => {
                    self.cloning = false;
                    self.clone_error = Some(e);
                }
                Msg::Dumped { index, id: Ok(id) } => {
                    let (pos, label) = if index < 3 {
                        let d = &self.replay["docs"][index];
                        let c = self.replay["clusters"].as_array().into_iter().flatten().find(|c| c["id"] == d["drop"]).cloned().unwrap_or(Value::Null);
                        ([(c["x"].as_f64().unwrap_or(0.5) + d["dx"].as_f64().unwrap_or(0.0)) as f32, (c["y"].as_f64().unwrap_or(0.5) + d["dy"].as_f64().unwrap_or(0.0)) as f32], d["path"].as_str().unwrap_or("").to_string())
                    } else if index == 3 {
                        ([0.50, 0.30], "thread: Clear the port queue".to_string())
                    } else {
                        ([0.50, 0.58], "note".to_string())
                    };
                    if index == 3 {
                        self.threads[0].dumped = Some(id.clone());
                        self.toast = Some((format!("Session stored as {id}. A ripple is sorting it."), self.now));
                    }
                    self.drops.push(Drop { at: self.now, dump: id, pos, label, reply: index });
                }
                Msg::Dumped { id: Err(e), .. } => self.dump_error = Some(e),
                Msg::Imported { batch, result } => {
                    self.importing = false;
                    match result {
                        Ok(note) => {
                            for id in std::mem::take(&mut self.pending) {
                                self.approved.insert(id);
                            }
                            self.import_note = Some((true, note));
                            self.toast = Some((if batch == 1 { "9 records and 5 clusters written to the soup.".into() } else { "2 records written to the soup.".into() }, self.now));
                        }
                        Err(e) => self.import_note = Some((false, e.chars().take(300).collect())),
                    }
                }
                Msg::Search { thread, item, rows, total } => {
                    if let Some(Item::Tool { rows: slot, .. }) = self.threads[thread].items.get_mut(item) {
                        *slot = Some((rows, total));
                    }
                    self.threads[thread].step_at = self.now;
                }
                Msg::Acts(s) => self.acts = Some(s),
                Msg::Ran { item, ok, tail } => {
                    if let Some(Item::Run { out, .. }) = self.threads[0].items.get_mut(item) {
                        *out = Some((ok, tail));
                    }
                    self.threads[0].step_at = self.now;
                    if !ok {
                        self.task_state = 2;
                        self.task_note = Some("Blocked: the command failed. See the thread.".into());
                    }
                }
            }
        }
    }

    /// The records each landed drop has revealed so far, by the ring reaching their dot.
    fn reveal(&mut self) {
        let speed = 0.20_f64;
        let mut seen: Vec<String> = Vec::new();
        for d in &self.drops {
            for id in self.replay["ripple_replies"][d.reply]["records"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
                let p = &self.replay["dots"][id];
                let (dx, dy) = (p[0].as_f64().unwrap_or(0.5) - d.pos[0] as f64, p[1].as_f64().unwrap_or(0.5) - d.pos[1] as f64);
                if self.now >= d.at + 0.5 + (dx * dx + dy * dy).sqrt() / speed {
                    seen.push(id.to_string());
                }
            }
        }
        for id in seen {
            if !self.approved.contains(&id) && !self.pending.contains(&id) {
                self.pending.push(id);
            }
        }
    }
    fn visible(&self, id: &str) -> bool {
        self.approved.contains(id) || self.pending.iter().any(|p| p == id)
    }
    fn links_ready(&self) -> bool {
        self.drops.len() >= 3 && self.visible("T-1") && self.visible("IS-2") && self.now > self.drops[2].at + 3.4
    }

    fn played_tokens(&self, adapter: &str) -> usize {
        self.threads.iter().flat_map(|t| t.items.iter()).map(|i| match i {
            Item::Agent { adapter: a, text, .. } if a == adapter => text.len(),
            Item::Peer { from, text, .. } if (from == "checker") == (adapter == "Gemini adapter") => text.len(),
            _ => 0,
        }).sum::<usize>() / 4
    }

    fn ripple_running(&self) -> bool {
        self.ingest_at.is_some() && (self.drops.len() < 3 || !self.links_ready()) || self.threads[0].dumped.is_some() && !self.visible("Q-1")
    }
}

impl ShellApp for App {
    fn frame_input(&mut self) -> FrameInput {
        FrameInput::new(self.env.clone())
    }

    fn ui(&mut self, ui: &mut Ui) {
        let mut mode = self.mode;
        // The shell's body is the whole window: this app paints its own bars.
        AppShell::new(WindowKey::main()).screen_mode(Bind::bound(&mut mode)).body(|ui, _, _| self.frame(ui)).show(ui, &mut ());
        self.mode = mode;
    }
}

impl App {
    fn frame(&mut self, ui: &mut Ui) {
        self.now = ui.input(|i| i.time);
        self.hits.clear();
        self.receive();
        self.reveal();
        let full = ui.max_rect();
        fill(ui, full, 0.0, BG);
        let bottom = Rect::from_min_max(pos2(full.left(), full.bottom() - 30.0), full.max);
        let body = Rect::from_min_max(full.min, pos2(full.right(), bottom.top()));
        match self.screen {
            Screen::Welcome => self.welcome(ui, body),
            Screen::Clone => self.clone_screen(ui, body),
            Screen::Ingest => self.ingest(ui, body),
            Screen::Main => self.main(ui, body),
        }
        // Status bar.
        fill(ui, bottom, 0.0, PANEL);
        ui.painter().line_segment([bottom.left_top(), bottom.right_top()], Stroke::new(1.0, DIVIDER));
        if self.screen == Screen::Main {
            let mut x = bottom.left() + 14.0;
            if self.ripple_running() {
                line(ui, pos2(x, bottom.top() + 7.0), "1 ripple running", 12.5, WARN_FG);
                x += 130.0;
            }
            let threads = self.threads.iter().filter(|t| t.step > 0).count();
            let waiting = self.threads.iter().filter(|t| t.step == 8 || t.step == 10).count();
            let parts = [format!("{} records to review", self.pending.len()), format!("{threads} threads · {waiting} waiting on you"), format!("{} records in the soup", self.approved.len())];
            line(ui, pos2(x, bottom.top() + 7.0), &parts.join("     "), 12.5, MUTED);
            ui.painter().text(bottom.right_center() - vec2(14.0, 0.0), Align2::RIGHT_CENTER, format!("{} · connected", self.name), font(12.5), MUTED);
        }
        // Toast.
        if let Some((s, t)) = &self.toast {
            if self.now - t < 4.0 {
                let w = measure(ui, s, font(13.0), 600.0).x + 28.0;
                let r = Rect::from_min_size(pos2(full.center().x - w / 2.0, bottom.top() - 120.0), vec2(w, 32.0));
                fill(ui, r, 16.0, Color32::from_rgb(0x28, 0x28, 0x28));
                outline(ui, r, 16.0, EDGE, 1.0);
                ui.painter().text(r.center(), Align2::CENTER_CENTER, s, font(13.0), TEXT);
            }
        }
        // The pointer, drawn so a screen recording shows it.
        if let Some(p) = ui.input(|i| i.pointer.latest_pos()).filter(|_| self.pointer) {
            let top = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("pointer")));
            top.circle_stroke(p, 9.0, Stroke::new(2.0, Color32::WHITE));
            top.circle_filled(p, 2.5, Color32::WHITE);
        }
        let ppp = ui.ctx().pixels_per_point();
        for v in self.hits.values_mut() {
            *v = [v[0] * ppp, v[1] * ppp];
        }
        self.write_hitmap();
        ui.ctx().request_repaint();
    }

    fn write_hitmap(&mut self) {
        let Some(path) = &self.hitmap else { return };
        let s = serde_json::json!({"hits": self.hits, "flags": self.flags}).to_string();
        if s != self.last_written {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, &s).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
            self.last_written = s;
        }
    }

    fn welcome(&mut self, ui: &mut Ui, body: Rect) {
        let c = body.center();
        if let Some(tex) = &self.tex {
            let r = Rect::from_center_size(pos2(c.x, body.top() + 190.0), vec2(448.0, 252.0));
            ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            outline(ui, r, 8.0, EDGE, 1.0);
        }
        ui.painter().text(pos2(c.x, body.top() + 352.0), Align2::CENTER_CENTER, "cauldron", font(30.0), TEXT);
        ui.painter().text(pos2(c.x, body.top() + 386.0), Align2::CENTER_CENTER, "A memory your coding agents read before they act.", font(14.0), MUTED);
        let y = body.top() + 430.0;
        if self.button(ui, "welcome_clone", Rect::from_min_size(pos2(c.x - 250.0, y), vec2(170.0, 34.0)), "Clone from GitHub", true) {
            self.screen = Screen::Clone;
        }
        let _ = self.button(ui, "welcome_open", Rect::from_min_size(pos2(c.x - 70.0, y), vec2(160.0, 34.0)), "Open a local folder", false);
        let _ = self.button(ui, "welcome_demo", Rect::from_min_size(pos2(c.x + 100.0, y), vec2(190.0, 34.0)), "Open the demo cauldron", false);
        ui.painter().text(pos2(c.x, y + 62.0), Align2::CENTER_CENTER, "Claude: not signed in   ·   Gemini: not signed in", font(12.0), MUTED);
    }

    fn clone_screen(&mut self, ui: &mut Ui, body: Rect) {
        let card = Rect::from_center_size(body.center(), vec2(620.0, 360.0));
        fill(ui, card, 8.0, PANEL);
        outline(ui, card, 8.0, EDGE, 1.0);
        let x = card.left() + 28.0;
        line(ui, pos2(x, card.top() + 22.0), "Clone from GitHub", 20.0, TEXT);
        let mut y = card.top() + 70.0;
        for (key, label, which) in [("clone_repo", "Repository", 0), ("clone_dest", "Destination", 1), ("clone_name", "Cauldron name", 2)] {
            line(ui, pos2(x, y + 7.0), label, 13.0, MUTED);
            let r = Rect::from_min_size(pos2(x + 130.0, y), vec2(430.0, 30.0));
            let value = match which {
                0 => &mut self.repo,
                1 => &mut self.dest,
                _ => &mut self.name,
            };
            let field = TextField::new(label, Bind::bound(value)).width_chars(48);
            let field = if which == 0 { field.placeholder("owner/repo") } else { field };
            let (_, used) = at(ui, r, |ui| {
                field.show(ui);
            });
            self.hit(key, used);
            y += 46.0;
        }
        y += 6.0;
        if self.cloning {
            at(ui, Rect::from_min_size(pos2(x, y), vec2(560.0, 14.0)), |ui| {
                Progress::indeterminate().show(ui);
            });
            line(ui, pos2(x, y + 24.0), &self.clone_line.clone(), 12.5, MUTED);
        } else {
            if self.button(ui, "clone_go", Rect::from_min_size(pos2(x, y), vec2(120.0, 34.0)), "Clone", true) {
                self.start_clone();
            }
            if self.button(ui, "clone_back", Rect::from_min_size(pos2(x + 100.0, y), vec2(120.0, 34.0)), "Back", false) {
                self.screen = Screen::Welcome;
            }
            if let Some(e) = self.clone_error.clone() {
                text(ui, pos2(x, y + 46.0), 560.0, &e, font(12.5), WARN_FG);
            }
        }
    }

    fn ingest(&mut self, ui: &mut Ui, body: Rect) {
        let card = Rect::from_center_size(body.center(), vec2(820.0, 640.0));
        fill(ui, card, 8.0, PANEL);
        outline(ui, card, 8.0, EDGE, 1.0);
        let x = card.left() + 28.0;
        line(ui, pos2(x, card.top() + 20.0), "Choose what it learns", 20.0, TEXT);
        let w = tag(ui, pos2(x, card.top() + 54.0), true);
        line(ui, pos2(x + w + 8.0, card.top() + 55.0), &self.setup_note.clone(), 12.0, MUTED);
        line(ui, pos2(x, card.top() + 78.0), "Documents found in the clone. Source code is never ingested.", 12.5, MUTED);
        let per_col = self.docs.len().div_ceil(2).max(1);
        for i in 0..self.docs.len() {
            let (col, row) = (i / per_col, i % per_col);
            let p = pos2(x + col as f32 * 390.0, card.top() + 108.0 + row as f32 * 26.0);
            let (_, used) = at(ui, Rect::from_min_size(p, vec2(24.0, 22.0)), |ui| {
                Checkbox::new(Bind::bound(&mut self.docs[i].ticked)).show(ui);
            });
            let key = format!("doc_{i}");
            self.hit(&key, used);
            let d = &self.docs[i];
            line(ui, p + vec2(28.0, 3.0), &d.path, 12.5, if d.ticked { TEXT } else { MUTED });
            let wname = measure(ui, &d.path, font(12.5), 400.0).x;
            line(ui, p + vec2(36.0 + wname, 4.0), d.reason, 11.0, if d.reason == "doc" { EDGE } else { Color32::from_rgb(0xba, 0xd6, 0xff) });
        }
        let n = self.docs.iter().filter(|d| d.ticked).count();
        let y = card.bottom() - 54.0;
        line(ui, pos2(x, y + 9.0), &format!("{n} documents selected"), 13.0, TEXT);
        if self.button(ui, "ingest_go", Rect::from_min_size(pos2(card.right() - 300.0, y), vec2(120.0, 34.0)), &format!("Ingest {n}"), true) && n > 0 {
            self.start_ingest();
        }
        if self.button(ui, "ingest_skip", Rect::from_min_size(pos2(card.right() - 180.0, y), vec2(150.0, 34.0)), "Skip, start empty", false) {
            self.screen = Screen::Main;
        }
    }

    fn main(&mut self, ui: &mut Ui, body: Rect) {
        // Title bar.
        let title = Rect::from_min_size(body.min, vec2(body.width(), 38.0));
        fill(ui, title, 0.0, PANEL);
        line(ui, title.left_top() + vec2(14.0, 10.0), "cauldron", 15.0, TEXT);
        let pick = Rect::from_min_size(title.left_top() + vec2(92.0, 7.0), vec2(118.0, 24.0));
        fill(ui, pick, 6.0, SUNKEN);
        outline(ui, pick, 6.0, EDGE, 1.0);
        ui.painter().text(pick.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, &self.name, font(12.5), TEXT);
        let tip = pick.right_center() - vec2(14.0, -3.0);
        ui.painter().add(Shape::convex_polygon(vec![tip + vec2(-4.5, -5.0), tip + vec2(4.5, -5.0), tip], MUTED, Stroke::NONE));
        let pools = format!("Claude pool ≈ {} tok   ·   Gemini pool ≈ {} tok", self.played_tokens("Claude adapter"), self.played_tokens("Gemini adapter"));
        ui.painter().text(title.right_center() - vec2(14.0, 0.0), Align2::RIGHT_CENTER, pools, font(12.0), MUTED);
        // Tabs.
        let tabs = Rect::from_min_size(title.left_bottom(), vec2(body.width(), 30.0));
        fill(ui, tabs, 0.0, PANEL);
        ui.painter().line_segment([tabs.left_bottom(), tabs.right_bottom()], Stroke::new(1.0, DIVIDER));
        ui.painter().line_segment([tabs.left_top(), tabs.right_top()], Stroke::new(1.0, DIVIDER));
        let mut x = tabs.left() + 8.0;
        for (key, label, tab) in [("tab_cauldron", "Cauldron", Tab::Cauldron), ("tab_threads", "Threads", Tab::Threads), ("tab_settings", "Settings", Tab::Settings)] {
            let w = measure(ui, label, font(13.0), 200.0).x + 26.0;
            let r = Rect::from_min_size(pos2(x, tabs.top()), vec2(w, 30.0));
            if self.tab == tab {
                fill(ui, r, 0.0, BG);
                ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(2.0, ACCENT));
            }
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, font(13.0), if self.tab == tab { TEXT } else { MUTED });
            if ui.interact(r, ui.id().with(key), Sense::click()).clicked() {
                self.tab = tab;
                if tab == Tab::Settings && self.acts.is_none() {
                    let (tx, bin, env) = (self.tx.clone(), self.cauldron_bin.clone(), self.cauldron_env.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(Msg::Acts(run_cli(&bin, &["owner-acts", "show", "--env", &env], None).unwrap_or_else(|e| e)));
                    });
                }
            }
            self.hit(key, r);
            x += w + 2.0;
        }
        let area = Rect::from_min_max(tabs.left_bottom(), body.max);
        match self.tab {
            Tab::Cauldron => self.cauldron_tab(ui, area),
            Tab::Threads => self.threads_tab(ui, area),
            Tab::Settings => self.settings_tab(ui, area),
        }
    }

    fn cauldron_tab(&mut self, ui: &mut Ui, area: Rect) {
        // Left rail: clusters.
        let rail = Rect::from_min_size(area.min, vec2(232.0, area.height()));
        fill(ui, rail, 0.0, PANEL);
        ui.painter().line_segment([rail.right_top(), rail.right_bottom()], Stroke::new(1.0, DIVIDER));
        line(ui, rail.left_top() + vec2(12.0, 9.0), "Clusters", 14.0, TEXT);
        let proposed = self.ingest_at.is_some_and(|t| self.now > t + 0.8);
        let accepted = self.approved.contains("DEC-1");
        let mut y = rail.top() + 40.0;
        if !proposed {
            line(ui, pos2(rail.left() + 12.0, y), "No clusters yet.", 12.5, MUTED);
        } else {
            let clusters = self.replay["clusters"].as_array().cloned().unwrap_or_default();
            for c in &clusters {
                let name = c["name"].as_str().unwrap_or("");
                let count = self.records.values().filter(|r| self.visible(r["id"].as_str().unwrap_or("")) && self.cluster_of(r) == name).count();
                line(ui, pos2(rail.left() + 12.0, y), name, 12.5, if accepted { TEXT } else { MUTED });
                ui.painter().text(pos2(rail.right() - 12.0, y + 8.0), Align2::RIGHT_CENTER, count.to_string(), font(12.0), MUTED);
                y += 26.0;
            }
            y += 6.0;
            if !accepted {
                text(ui, pos2(rail.left() + 12.0, y), rail.width() - 24.0, "Proposed by the ripple. They become clusters when you approve.", font(11.5), MUTED);
            }
        }
        // Sub-tabs.
        let rest = Rect::from_min_max(pos2(rail.right(), area.top()), area.max);
        let bar = Rect::from_min_size(rest.min, vec2(rest.width(), 34.0));
        fill(ui, bar, 0.0, PANEL);
        ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, DIVIDER));
        let mut x = bar.left() + 10.0;
        let tasks = if self.approved.contains("T-1") { 1 } else { 0 };
        for (key, label, sub) in [("sub_surface", "Surface".to_string(), Sub::Surface), ("sub_review", format!("Review {}", self.pending.len()), Sub::Review), ("sub_tasks", format!("Tasks {tasks}"), Sub::Tasks)] {
            let w = measure(ui, &label, font(12.5), 200.0).x + 22.0;
            let r = Rect::from_min_size(pos2(x, bar.top() + 5.0), vec2(w, 24.0));
            if self.sub == sub {
                fill(ui, r, 12.0, SELECTED);
            }
            ui.painter().text(r.center(), Align2::CENTER_CENTER, &label, font(12.5), if self.sub == sub { TEXT } else { MUTED });
            if ui.interact(r, ui.id().with(key), Sense::click()).clicked() {
                self.sub = sub;
            }
            self.hit(key, r);
            x += w + 6.0;
        }
        // Ripple status, right side of the bar.
        if self.ripple_running() {
            at(ui, Rect::from_min_size(pos2(bar.right() - 250.0, bar.top() + 5.0), vec2(24.0, 24.0)), |ui| {
                Spinner::new().show(ui);
            });
            line(ui, pos2(bar.right() - 220.0, bar.top() + 9.0), "ripple running", 12.5, WARN_FG);
        } else if !self.pending.is_empty() {
            line(ui, pos2(bar.right() - 220.0, bar.top() + 9.0), &format!("{} records ready to review", self.pending.len()), 12.5, WARN_FG);
        }
        self.flag("review_ready", self.links_ready() && self.pending.len() >= 9);
        self.flag("approved_1", self.approved.contains("DEC-1"));
        self.flag("approved_2", self.approved.contains("Q-1"));
        self.flag("session_ready", self.pending.iter().any(|p| p == "Q-1") && self.pending.iter().any(|p| p == "DEC-2"));
        let bucket = Rect::from_min_max(pos2(rest.left() + 12.0, rest.bottom() - 62.0), pos2(rest.right() - 12.0, rest.bottom() - 10.0));
        fill(ui, bucket, 8.0, SUNKEN);
        outline(ui, bucket, 8.0, WARN, 1.0);
        line(ui, bucket.left_top() + vec2(12.0, 5.0), "Bucket", 11.0, WARN_FG);
        let (_, used) = at(ui, Rect::from_min_size(bucket.left_top() + vec2(10.0, 20.0), vec2(bucket.width() - 110.0, 28.0)), |ui| {
            TextField::new("Bucket", Bind::bound(&mut self.note)).placeholder("Dump a thought, a decision, anything").width_chars(110).show(ui);
        });
        self.hit("bucket", used);
        if self.button(ui, "bucket_dump", Rect::from_min_size(pos2(bucket.right() - 84.0, bucket.top() + 12.0), vec2(74.0, 30.0)), "Dump", true) && !self.note.trim().is_empty() {
            let file = std::env::temp_dir().join("cauldron-gui-note.md");
            let _ = std::fs::write(&file, std::mem::take(&mut self.note));
            let (tx, bin, env, name) = (self.tx.clone(), self.cauldron_bin.clone(), self.cauldron_env.clone(), self.name.clone());
            std::thread::spawn(move || {
                let out = run_cli(&bin, &["soup", "dump", &file.to_string_lossy(), "--cauldron", &name, "--env", &env, "--title", "note", "--source", "bucket"], None);
                let id = out.and_then(|o| serde_json::from_str::<Value>(&o).ok().and_then(|v| v["dump_id"].as_str().map(String::from)).ok_or_else(|| "no dump id in the receipt".to_string()));
                let _ = tx.send(Msg::Dumped { index: 9, id });
            });
        }
        let content = Rect::from_min_max(bar.left_bottom(), pos2(rest.right(), bucket.top() - 10.0));
        match self.sub {
            Sub::Surface => self.surface(ui, content),
            Sub::Review => self.review(ui, content),
            Sub::Tasks => self.tasks(ui, content),
        }
    }

    fn surface(&mut self, ui: &mut Ui, content: Rect) {
        let side_w = if self.board_open { 400.0 } else { 300.0 };
        let canvas = Rect::from_min_max(content.min, pos2(content.right() - side_w, content.bottom()));
        let side = Rect::from_min_max(pos2(canvas.right(), content.top()), content.max);
        fill(ui, canvas, 0.0, SOUP);
        let p = ui.painter().with_clip_rect(canvas);
        let to = |x: f64, y: f64| pos2(canvas.left() + x as f32 * canvas.width(), canvas.top() + y as f32 * canvas.height());
        let accepted = self.approved.contains("DEC-1");
        if self.ingest_at.is_none() {
            p.text(canvas.center(), Align2::CENTER_CENTER, "The surface is calm. Nothing is pending.", font(14.0), EDGE);
        }
        // Cluster areas.
        if let Some(t0) = self.ingest_at {
            let fade = ((self.now - t0 - 0.4) / 0.8).clamp(0.0, 1.0) as f32;
            for c in self.replay["clusters"].as_array().into_iter().flatten() {
                let (centre, r) = (to(c["x"].as_f64().unwrap_or(0.5), c["y"].as_f64().unwrap_or(0.5)), c["r"].as_f64().unwrap_or(0.1) as f32 * canvas.width());
                let col = Color32::from_rgba_unmultiplied(0xd4, 0xd4, 0xd4, (if accepted { 150.0 } else { 90.0 } * fade) as u8);
                if accepted {
                    p.circle_stroke(centre, r, Stroke::new(1.2, col));
                } else {
                    let pts: Vec<Pos2> = (0..=72).map(|i| centre + r * vec2((i as f32 * 5.0).to_radians().cos(), (i as f32 * 5.0).to_radians().sin())).collect();
                    p.extend(Shape::dashed_line(&pts, Stroke::new(1.0, col), 6.0, 5.0));
                }
                p.text(centre - vec2(0.0, r + 10.0), Align2::CENTER_CENTER, c["name"].as_str().unwrap_or(""), font(12.0), Color32::from_rgba_unmultiplied(0xee, 0xee, 0xee, (220.0 * fade) as u8));
            }
        }
        // Drops and their rings.
        for d in &self.drops {
            let (centre, age) = (to(d.pos[0] as f64, d.pos[1] as f64), (self.now - d.at) as f32);
            for k in 0..3 {
                let a = age - k as f32 * 0.7;
                if a > 0.0 && a < 5.0 {
                    let alpha = (1.0 - a / 5.0).powf(1.5) * 150.0;
                    p.circle_stroke(centre, a * 0.20 * canvas.width(), Stroke::new(1.2, Color32::from_rgba_unmultiplied(255, 255, 255, alpha as u8)));
                }
            }
            let fall = (age / 0.35).clamp(0.0, 1.0);
            if age < 5.0 {
                p.circle_filled(centre - vec2(0.0, (1.0 - fall) * 60.0), 3.5, Color32::from_rgba_unmultiplied(0xfc, 0xcd, 0x78, ((1.0 - age / 5.0) * 255.0) as u8));
            }
            if age < 6.0 {
                let right = d.pos[0] > 0.6;
                p.text(centre + vec2(if right { -10.0 } else { 10.0 }, 18.0), if right { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER }, format!("{} · {}", d.dump, d.label), font(11.0), Color32::from_rgba_unmultiplied(0xfc, 0xcd, 0x78, ((1.0 - age / 6.0) * 255.0) as u8));
            }
        }
        // Links, then the conflict.
        let dot = |id: &str| {
            let d = &self.replay["dots"][id];
            to(d[0].as_f64().unwrap_or(0.5), d[1].as_f64().unwrap_or(0.5))
        };
        let mut conflict: Option<Rect> = None;
        let links_on = self.links_ready();
        for (id, r) in &self.records {
            for e in r["edges"].as_array().into_iter().flatten().filter(|e| e["kind"] == "ref") {
                let to_id = e["to"].as_str().unwrap_or("");
                if !self.visible(id) || !self.visible(to_id) || (!links_on && !self.approved.contains(id) && id != "DEC-2" && id != "Q-1") {
                    continue;
                }
                let (a, b) = (dot(id), dot(to_id));
                let is_conflict = e["note"] == "conflicts with";
                if is_conflict && !(self.now > self.drops.get(2).map_or(f64::MAX, |d| d.at) + 4.4) {
                    continue;
                }
                if is_conflict {
                    let pulse = 0.6 + 0.4 * ((self.now * 3.0).sin() as f32);
                    p.line_segment([a, b], Stroke::new(2.5, if self.flagged { WARN_EDGE } else { WARN.gamma_multiply(pulse) }));
                    let mid = a + (b - a) * 0.5;
                    p.text(mid - vec2(14.0, 0.0), Align2::RIGHT_CENTER, if self.flagged { "settled: kept, flagged" } else { "conflict · open the board" }, font(11.5), WARN_FG);
                    conflict = Some(Rect::from_center_size(mid, vec2(36.0, 36.0)));
                } else {
                    p.line_segment([a, b], Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 70)));
                }
            }
        }
        // Dots.
        for id in self.records.keys() {
            if !self.visible(id) {
                continue;
            }
            let c = dot(id);
            p.circle_filled(c, 9.0, Color32::from_rgba_unmultiplied(255, 255, 255, 18));
            if self.approved.contains(id) {
                p.circle_filled(c, 4.5, Color32::WHITE);
            } else {
                p.circle_filled(c, 4.5, SOUP);
                p.circle_stroke(c, 4.5, Stroke::new(1.5, Color32::WHITE));
            }
            p.text(c + vec2(9.0, 9.0), Align2::LEFT_CENTER, id, font(11.0), MUTED);
        }
        if let Some(r) = conflict {
            if ui.interact(r, ui.id().with("conflict"), Sense::click()).clicked() && !self.board_open {
                self.board_open = true;
                self.board_at = self.now;
            }
            self.hit("conflict", r);
            self.flag("conflict_ready", true);
        }
        // Legend.
        let lg = canvas.left_bottom() + vec2(14.0, -52.0);
        p.circle_stroke(lg + vec2(5.0, 8.0), 4.5, Stroke::new(1.5, Color32::WHITE));
        p.text(lg + vec2(16.0, 8.0), Align2::LEFT_CENTER, "unreviewed", font(11.5), MUTED);
        p.circle_filled(lg + vec2(5.0, 28.0), 4.5, Color32::WHITE);
        p.text(lg + vec2(16.0, 28.0), Align2::LEFT_CENTER, "approved, in the soup", font(11.5), MUTED);
        if let Some(e) = &self.dump_error {
            p.text(canvas.left_top() + vec2(14.0, 14.0), Align2::LEFT_TOP, format!("dump failed: {e}"), font(12.0), WARN_FG);
        }
        // Side: the board, or the plain list of records.
        fill(ui, side, 0.0, PANEL);
        ui.painter().line_segment([side.left_top(), side.left_bottom()], Stroke::new(1.0, DIVIDER));
        if self.board_open { self.board(ui, side) } else { self.record_list(ui, side) }
    }

    fn record_list(&mut self, ui: &mut Ui, side: Rect) {
        line(ui, side.left_top() + vec2(12.0, 9.0), "Ledger", 14.0, TEXT);
        let mut y = side.top() + 38.0;
        if self.drops.is_empty() {
            text(ui, pos2(side.left() + 12.0, y), side.width() - 24.0, "Nothing yet. Each dump appears here with what it produced.", font(12.0), MUTED);
        }
        for d in self.drops.iter().rev() {
            let ids: Vec<String> = self.replay["ripple_replies"][d.reply]["records"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
            let seen = ids.iter().filter(|i| self.visible(i)).count();
            let done = !ids.is_empty() && ids.iter().all(|i| self.approved.contains(i));
            let running = seen < ids.len();
            let col = if done { GOOD_FG } else if ids.is_empty() { MUTED } else { WARN };
            ui.painter().circle_filled(pos2(side.left() + 17.0, y + 8.0), 4.0, col);
            line(ui, pos2(side.left() + 30.0, y), &format!("{} · {}", d.dump, d.label), 12.5, TEXT);
            let state = if ids.is_empty() { "received".to_string() } else if running { format!("ripple running · {seen} records so far") } else if done { format!("{} records approved", ids.len()) } else { format!("{} records to review", ids.len()) };
            line(ui, pos2(side.left() + 30.0, y + 18.0), &state, 11.5, MUTED);
            y += 44.0;
        }
        y += 4.0;
        ui.painter().line_segment([pos2(side.left(), y), pos2(side.right(), y)], Stroke::new(1.0, DIVIDER));
        line(ui, pos2(side.left() + 12.0, y + 9.0), "Records", 14.0, TEXT);
        y += 36.0;
        let ids: Vec<String> = self.approved.iter().cloned().chain(self.pending.iter().cloned()).collect();
        for id in ids {
            let r = &self.records[&id];
            let done = self.approved.contains(&id);
            ui.painter().text(pos2(side.left() + 12.0, y), Align2::LEFT_TOP, &id, font(11.5), if done { GOOD_FG } else { WARN_FG });
            let h = text(ui, pos2(side.left() + 62.0, y), side.width() - 74.0, r["handle"].as_str().unwrap_or(""), font(11.5), if done { TEXT } else { MUTED });
            y += h + 6.0;
        }
    }

    fn board(&mut self, ui: &mut Ui, side: Rect) {
        let x = side.left() + 12.0;
        let w = side.width() - 24.0;
        line(ui, side.left_top() + vec2(12.0, 9.0), "Ripples · dumps DM-1 to DM-3", 14.0, TEXT);
        tag(ui, pos2(side.right() - 70.0, side.top() + 10.0), false);
        let mut y = side.top() + 40.0;
        // Members.
        let members = self.replay["members"].as_array().cloned().unwrap_or_default();
        for (i, m) in members.iter().enumerate() {
            let r = Rect::from_min_size(pos2(x + (i % 2) as f32 * (w / 2.0 + 3.0), y + (i / 2) as f32 * 44.0), vec2(w / 2.0 - 3.0, 38.0));
            fill(ui, r, 6.0, SUNKEN);
            line(ui, r.left_top() + vec2(8.0, 4.0), m[0].as_str().unwrap_or(""), 12.5, TEXT);
            let warn = m[2] == "warn" && !self.flagged && self.now > self.board_at + 5.4;
            let state = if m[2] == "warn" && !warn { if self.flagged { "settled" } else { "checking" } } else { m[1].as_str().unwrap_or("") };
            line(ui, r.left_top() + vec2(8.0, 20.0), state, 11.5, if warn { WARN_FG } else if m[2] == "good" { GOOD_FG } else { MUTED });
        }
        y += 96.0;
        // Posts.
        let posts = self.replay["posts"].as_array().cloned().unwrap_or_default();
        let mut shown = 0;
        let mut n = 0;
        for post in &posts {
            let after = post["after_reply"] == true;
            let visible = if after { self.replied.as_ref().is_some_and(|(_, t)| self.now > t + 1.6) } else { self.now > self.board_at + 0.8 + 1.5 * n as f64 };
            if after && self.replied.is_some() && n == 4 {
                // The owner's own post sits before the verifier's answer.
                let (s, _) = self.replied.clone().unwrap_or_default();
                y += self.post(ui, pos2(x, y), w, 5, "you", "@verifier", &s, false, true) + 8.0;
            }
            n += 1;
            if !visible {
                continue;
            }
            shown += 1;
            let number = if after { 6 } else { n };
            y += self.post(ui, pos2(x, y), w, number, post["from"].as_str().unwrap_or(""), post["to"].as_str().unwrap_or(""), post["text"].as_str().unwrap_or(""), post["warn"] == true && !self.flagged, false) + 8.0;
            if after {
                self.flagged = true;
            }
        }
        self.flag("board_asked", shown >= 4);
        self.flag("board_done", self.flagged);
        // Reply.
        let ry = side.bottom() - 44.0;
        if self.replied.is_none() {
            let (_, used) = at(ui, Rect::from_min_size(pos2(x, ry), vec2(w - 70.0, 30.0)), |ui| {
                TextField::new("Post to the board", Bind::bound(&mut self.reply)).placeholder("Reply to the board").width_chars(34).show(ui);
            });
            self.hit("board_reply", used);
            if self.button(ui, "board_post", Rect::from_min_size(pos2(side.right() - 72.0, ry), vec2(60.0, 30.0)), "Post", false) && !self.reply.trim().is_empty() {
                self.replied = Some((std::mem::take(&mut self.reply), self.now));
            }
        } else {
            line(ui, pos2(x, ry + 8.0), "Your post is on the board.", 12.0, MUTED);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn post(&mut self, ui: &mut Ui, pos: Pos2, w: f32, number: usize, from: &str, to: &str, body: &str, warn: bool, mine: bool) -> f32 {
        let h = measure(ui, body, font(12.0), w - 20.0).y + 30.0;
        let r = Rect::from_min_size(pos, vec2(w, h));
        fill(ui, r, 8.0, if warn { WARN_BG } else if mine { ACCENT_STRONG } else { BG });
        if warn {
            outline(ui, r, 8.0, WARN_EDGE, 1.0);
        }
        line(ui, pos + vec2(10.0, 6.0), &format!("#{number}  {from} → {to}"), 11.0, if warn { WARN_FG } else { MUTED });
        if mine {
            tag(ui, pos2(r.right() - 48.0, pos.y + 5.0), true);
        }
        text(ui, pos + vec2(10.0, 22.0), w - 20.0, body, font(12.0), TEXT);
        h
    }

    fn review(&mut self, ui: &mut Ui, content: Rect) {
        let x = content.left() + 16.0;
        let n = self.pending.len();
        let head = if n > 0 { format!("{n} records waiting. Nothing is written until you approve.") } else if self.approved.is_empty() { "Nothing to review yet.".to_string() } else { "Nothing waiting. Everything shown is in the soup.".to_string() };
        line(ui, pos2(x, content.top() + 14.0), &head, 15.0, TEXT);
        if n > 0 && !self.importing {
            let label = format!("Approve all {n}");
            if self.button(ui, "review_approve", Rect::from_min_size(pos2(content.right() - 160.0, content.top() + 8.0), vec2(140.0, 32.0)), &label, true) {
                self.approve();
            }
        }
        if let Some((ok, note)) = self.import_note.clone() {
            let w = tag(ui, pos2(x, content.top() + 42.0), true);
            text(ui, pos2(x + w + 8.0, content.top() + 43.0), content.width() - 240.0, &note, font(12.0), if ok { GOOD_FG } else { WARN_FG });
        }
        let ids: Vec<String> = if n > 0 { self.pending.clone() } else { self.approved.iter().cloned().collect() };
        let (cw, ch) = ((content.width() - 44.0) / 2.0, 96.0);
        for (i, id) in ids.iter().enumerate().take(10) {
            let r = &self.records[id];
            let card = Rect::from_min_size(pos2(x + (i % 2) as f32 * (cw + 12.0), content.top() + 68.0 + (i / 2) as f32 * (ch + 6.0)), vec2(cw, ch));
            fill(ui, card, 8.0, PANEL);
            outline(ui, card, 8.0, EDGE, 1.0);
            let dump = r["meta"]["cites"][0]["id"].as_str().unwrap_or("");
            let done = self.approved.contains(id);
            line(ui, card.left_top() + vec2(12.0, 8.0), &format!("{} · {id} → {} · from {dump}", r["type"].as_str().unwrap_or(""), self.cluster_of(r)), 11.0, MUTED);
            tag(ui, pos2(card.right() - 62.0, card.top() + 7.0), done);
            let strip = Rect::from_min_size(card.left_top() + vec2(12.0, 26.0), vec2(cw - 24.0, 22.0));
            fill(ui, strip, 6.0, GOOD_BG);
            outline(ui, strip, 6.0, GOOD_EDGE, 1.0);
            ui.painter().with_clip_rect(strip).text(strip.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, format!("+ {}", r["handle"].as_str().unwrap_or("")), mono(11.5), GOOD_FG);
            let flagged = id == "IS-2" && self.flagged;
            let body = r["body"].as_str().unwrap_or("");
            ui.painter().with_clip_rect(Rect::from_min_size(card.left_top() + vec2(12.0, 53.0), vec2(cw - 24.0, if flagged { 16.0 } else { 32.0 }))).galley(card.left_top() + vec2(12.0, 53.0), ui.painter().layout(body.to_owned(), font(11.5), MUTED, cw - 24.0), MUTED);
            if flagged {
                line(ui, card.left_bottom() + vec2(12.0, -22.0), "@verifier: conflicts with CON-3. Kept as an issue, flagged \"not evidence\".", 11.0, WARN_FG);
            }
        }
    }

    fn tasks(&mut self, ui: &mut Ui, content: Rect) {
        let cols = ["Proposed", "Running", "Needs you", "Done"];
        let cw = (content.width() - 32.0 - 36.0) / 4.0;
        for (i, name) in cols.iter().enumerate() {
            let r = Rect::from_min_size(pos2(content.left() + 16.0 + i as f32 * (cw + 12.0), content.top() + 16.0), vec2(cw, content.height() - 32.0));
            fill(ui, r, 8.0, SUNKEN);
            line(ui, r.left_top() + vec2(12.0, 10.0), name, 13.0, TEXT);
            if self.approved.contains("T-1") && self.task_state == i {
                let card = Rect::from_min_size(r.left_top() + vec2(8.0, 38.0), vec2(cw - 16.0, 118.0));
                fill(ui, card, 8.0, PANEL);
                outline(ui, card, 8.0, if i == 2 { WARN } else { EDGE }, 1.0);
                line(ui, card.left_top() + vec2(10.0, 8.0), "T-1 · Matching decompilation", 11.0, MUTED);
                text(ui, card.left_top() + vec2(10.0, 26.0), cw - 36.0, self.records["T-1"]["handle"].as_str().unwrap_or(""), font(12.5), TEXT);
                let note = self.task_note.clone().unwrap_or_else(|| ["From dump DM-3. Not started.", "Running in thread \"Clear the port queue\".", "A thread is asking for permission.", ""][i].to_string());
                let note = note.as_str();
                text(ui, card.left_top() + vec2(10.0, 80.0), cw - 36.0, note, font(11.0), if i == 2 { WARN_FG } else { MUTED });
            }
        }
        if !self.approved.contains("T-1") {
            line(ui, pos2(content.left() + 28.0, content.top() + 60.0), "Tasks appear once their records are approved.", 12.0, MUTED);
        }
    }

    fn settings_tab(&mut self, ui: &mut Ui, area: Rect) {
        let x = area.left() + 40.0;
        let w = (area.width() - 80.0).min(900.0);
        line(ui, pos2(x, area.top() + 22.0), "Settings", 20.0, TEXT);
        let acts: Value = self.acts.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
        let mine = acts["cauldrons"].as_array().into_iter().flatten().find(|c| c["cauldron"] == self.name.as_str() || c["name"] == self.name.as_str()).or_else(|| acts["cauldrons"].get(0)).cloned().unwrap_or(Value::Null);
        let act = |k: &str| mine["acts"][k].as_str().unwrap_or("unknown").to_string();
        let where_ = |v: &str| if v == "terminal" { "only you, at a terminal".to_string() } else { v.to_string() };
        let rows: Vec<(&str, String, String, bool)> = vec![
            ("Accounts", "Claude pool: no account signed in".into(), format!("≈ {} tokens used this session. Sign in to add an account to the pool.", self.played_tokens("Claude adapter")), false),
            ("", "Gemini pool: no account signed in".into(), format!("≈ {} tokens used this session. Sign in to add an account to the pool.", self.played_tokens("Gemini adapter")), false),
            ("Agents", format!("Who may grant an agent access to this cauldron: {}", where_(&act("grant"))), format!("To grant Claude Code: cauldron mcp grant {} --env {}", self.name, self.cauldron_env), true),
            ("Ripples", format!("Who may turn automatic ripples on: {}", where_(&act("ripples"))), format!("Automatic ripples are off. To turn on: cauldron ripples on {} --env {}", self.name, self.cauldron_env), true),
            ("Approval", format!("Who may approve records: {}", where_(&act("approve"))), "Agents cannot approve for you unless you open that act to them.".into(), true),
        ];
        let mut y = area.top() + 70.0;
        for (section, title, detail, real) in rows {
            let r = Rect::from_min_size(pos2(x, y), vec2(w, 62.0));
            fill(ui, r, 8.0, PANEL);
            outline(ui, r, 8.0, EDGE, 1.0);
            line(ui, r.left_top() + vec2(14.0, 10.0), section, 12.0, MUTED);
            line(ui, r.left_top() + vec2(120.0, 9.0), &title, 13.5, TEXT);
            line(ui, r.left_top() + vec2(120.0, 34.0), &detail, 11.5, MUTED);
            if real {
                tag(ui, pos2(r.right() - 60.0, r.top() + 10.0), true);
            }
            y += 70.0;
        }
        text(ui, pos2(x, y + 6.0), w, "Granting access, automatic ripples and approval are owner acts. To change one, run the command shown in a terminal.", font(12.0), MUTED);
        self.flag("settings_loaded", self.acts.is_some());
    }

    fn threads_tab(&mut self, ui: &mut Ui, area: Rect) {
        // Thread list.
        let rail = Rect::from_min_size(area.min, vec2(232.0, area.height()));
        fill(ui, rail, 0.0, PANEL);
        ui.painter().line_segment([rail.right_top(), rail.right_bottom()], Stroke::new(1.0, DIVIDER));
        line(ui, rail.left_top() + vec2(12.0, 9.0), "Threads", 14.0, TEXT);
        let mut y = rail.top() + 40.0;
        for i in 0..self.threads.len() {
            if i > 0 && self.threads[i].step == 0 && !self.threads[0].done {
                continue;
            }
            let r = Rect::from_min_size(pos2(rail.left() + 6.0, y), vec2(rail.width() - 12.0, 30.0));
            if self.cur == i {
                fill(ui, r, 6.0, SELECTED);
            }
            let title = if i > 0 && self.threads[i].step == 0 { "+ New thread".to_string() } else { self.threads[i].title.clone() };
            ui.painter().text(r.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, title, font(12.5), TEXT);
            let key = format!("thread_{i}");
            if ui.interact(r, ui.id().with(&key), Sense::click()).clicked() {
                self.cur = i;
            }
            self.hit(&key, r);
            y += 34.0;
        }
        let main = Rect::from_min_max(pos2(rail.right(), area.top()), area.max);
        self.advance_thread();
        // Header: agents on this thread.
        let head = Rect::from_min_size(main.min, vec2(main.width(), 40.0));
        fill(ui, head, 0.0, PANEL);
        ui.painter().line_segment([head.left_bottom(), head.right_bottom()], Stroke::new(1.0, DIVIDER));
        let mut hx = head.left() + 14.0;
        for (name, adapter) in [("planner", "Claude adapter"), ("checker", "Gemini adapter")] {
            if self.cur == 1 && name == "checker" {
                continue;
            }
            let label = format!("{name} · {adapter} · ≈ {} tok", if self.cur == 0 || name == "planner" { self.played_tokens(adapter) } else { 0 });
            let w = measure(ui, &label, font(12.0), 400.0).x + 20.0;
            let r = Rect::from_min_size(pos2(hx, head.top() + 8.0), vec2(w, 24.0));
            fill(ui, r, 12.0, SUNKEN);
            outline(ui, r, 12.0, EDGE, 1.0);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, font(12.0), TEXT);
            hx += w + 8.0;
        }
        let t_done = self.threads[self.cur].done;
        let dumped = self.threads[self.cur].dumped.clone();
        if self.cur == 0 && t_done {
            if let Some(id) = dumped {
                let w = tag(ui, pos2(head.right() - 250.0, head.top() + 12.0), true);
                line(ui, pos2(head.right() - 244.0 + w, head.top() + 12.0), &format!("session stored as {id}"), 12.0, GOOD_FG);
            } else if self.button(ui, "thread_dump", Rect::from_min_size(pos2(head.right() - 150.0, head.top() + 5.0), vec2(130.0, 30.0)), "Dump session", true) {
                self.dump_session();
            }
        }
        // Prompt.
        let pr = Rect::from_min_max(pos2(main.left(), main.bottom() - 50.0), main.max);
        fill(ui, pr, 0.0, PANEL);
        ui.painter().line_segment([pr.left_top(), pr.right_top()], Stroke::new(1.0, DIVIDER));
        if self.threads[self.cur].step == 0 {
            let (_, used) = at(ui, Rect::from_min_size(pr.left_top() + vec2(14.0, 10.0), vec2(pr.width() - 110.0, 30.0)), |ui| {
                TextField::new("Message", Bind::bound(&mut self.prompt)).placeholder("Ask the agents").width_chars(96).show(ui);
            });
            self.hit("thread_prompt", used);
            if self.button(ui, "thread_send", Rect::from_min_size(pos2(pr.right() - 84.0, pr.top() + 10.0), vec2(70.0, 30.0)), "Send", true) && !self.prompt.trim().is_empty() {
                let p = std::mem::take(&mut self.prompt);
                let now = self.now;
                let t = &mut self.threads[self.cur];
                t.items.push(Item::User(p));
                t.step = 1;
                t.step_at = now;
            }
        } else {
            line(ui, pr.left_top() + vec2(14.0, 17.0), if t_done { "This thread is finished." } else { "The agents are working…" }, 12.5, MUTED);
        }
        // Conversation, anchored to the bottom.
        let conv = Rect::from_min_max(head.left_bottom(), pr.right_top());
        let width = (conv.width() - 60.0).min(820.0);
        let x = conv.left() + 30.0;
        let total: f32 = (0..self.threads[self.cur].items.len()).map(|i| self.item(ui, i, pos2(x, 0.0), width, false) + 12.0).sum();
        let mut y = if total > conv.height() - 24.0 { conv.bottom() - 12.0 - total } else { conv.top() + 14.0 };
        let clip = ui.clip_rect();
        ui.set_clip_rect(conv);
        for i in 0..self.threads[self.cur].items.len() {
            y += self.item(ui, i, pos2(x, y), width, true) + 12.0;
        }
        ui.set_clip_rect(clip);
        self.flag("thread_idle", self.threads[self.cur].step == 0);
        self.flag("perm_ready", self.cur == 0 && self.threads[0].step == 8);
        self.flag("visual_ready", self.cur == 0 && self.threads[0].step == 10);
        self.flag("thread0_done", self.threads[0].done);
        self.flag("thread1_done", self.threads[1].done);
        self.flag("session_dumped", self.threads[0].dumped.is_some());
    }

    /// Lays out (and, with `paint`, draws) conversation item `i`; returns its height.
    fn item(&mut self, ui: &mut Ui, i: usize, pos: Pos2, width: f32, paint: bool) -> f32 {
        let cur = self.cur;
        // Cards are interactive, so they are handled apart from the borrowed item.
        if let Item::Card(which) = self.threads[cur].items[i] {
            return self.card(ui, which, pos, width.min(640.0), paint);
        }
        match &self.threads[cur].items[i] {
            Item::User(s) => {
                let size = measure(ui, s, font(13.0), width * 0.7);
                let r = Rect::from_min_size(pos2(pos.x + width - size.x - 24.0, pos.y), size + vec2(24.0, 16.0));
                if paint {
                    fill(ui, r, 8.0, ACCENT_STRONG);
                    text(ui, r.left_top() + vec2(12.0, 8.0), width * 0.7, s, font(13.0), Color32::WHITE);
                }
                r.height()
            }
            Item::Agent { who, adapter, text: body } => {
                let h = measure(ui, body, font(13.0), width - 24.0).y + 40.0;
                if paint {
                    let r = Rect::from_min_size(pos, vec2(width, h));
                    fill(ui, r, 8.0, PANEL);
                    line(ui, pos + vec2(12.0, 8.0), &format!("{who} · {adapter}"), 11.5, MUTED);
                    tag(ui, pos2(r.right() - 66.0, pos.y + 7.0), false);
                    text(ui, pos + vec2(12.0, 28.0), width - 24.0, body, font(13.0), TEXT);
                }
                h
            }
            Item::Tool { who, adapter, query, rows } => {
                let n = rows.as_ref().map_or(1, |r| r.0.len().max(1));
                let h = 34.0 + n as f32 * 20.0 + 6.0;
                if paint {
                    let r = Rect::from_min_size(pos, vec2(width, h));
                    fill(ui, r, 6.0, SOUP);
                    outline(ui, r, 6.0, DIVIDER, 1.0);
                    ui.painter().text(pos + vec2(12.0, 9.0), Align2::LEFT_TOP, format!("{who} ({adapter}) → cauldron soup search \"{query}\""), mono(12.0), MUTED);
                    tag(ui, pos2(r.right() - 52.0, pos.y + 8.0), true);
                    match rows {
                        None => line(ui, pos + vec2(12.0, 32.0), "searching the soup…", 12.0, MUTED),
                        Some((rows, _)) if rows.is_empty() => line(ui, pos + vec2(12.0, 32.0), "no records matched", 12.0, WARN_FG),
                        Some((rows, _)) => {
                            for (k, row) in rows.iter().enumerate() {
                                let y = pos.y + 32.0 + k as f32 * 20.0;
                                ui.painter().text(pos2(pos.x + 12.0, y), Align2::LEFT_TOP, &row[0], mono(12.0), GOOD_FG);
                                ui.painter().with_clip_rect(r).text(pos2(pos.x + 76.0, y), Align2::LEFT_TOP, format!("{} · {}", row[1], row[2]), font(12.0), TEXT);
                            }
                        }
                    }
                }
                h
            }
            Item::Peer { from, to, text: body } => {
                let w = width * 0.78;
                let h = measure(ui, body, font(12.5), w - 24.0).y + 36.0;
                if paint {
                    let left = from == "planner";
                    let r = Rect::from_min_size(pos2(if left { pos.x } else { pos.x + width - w }, pos.y), vec2(w, h));
                    fill(ui, r, 8.0, SUNKEN);
                    outline(ui, r, 8.0, if left { EDGE } else { ACCENT }, 1.0);
                    let adapter = if left { "Claude adapter" } else { "Gemini adapter" };
                    line(ui, r.left_top() + vec2(12.0, 7.0), &format!("{from} ({adapter}) → {to} · agent message"), 11.0, MUTED);
                    tag(ui, pos2(r.right() - 66.0, r.top() + 6.0), false);
                    text(ui, r.left_top() + vec2(12.0, 26.0), w - 24.0, body, font(12.5), TEXT);
                }
                h
            }
            Item::Note(s) => {
                if paint {
                    line(ui, pos, s, 12.0, MUTED);
                }
                18.0
            }
            Item::Run { cmd, out } => {
                let n = out.as_ref().map_or(1, |o| o.1.len().max(1));
                let h = 34.0 + n as f32 * 18.0 + 8.0;
                if paint {
                    let r = Rect::from_min_size(pos, vec2(width, h));
                    fill(ui, r, 6.0, SOUP);
                    outline(ui, r, 6.0, DIVIDER, 1.0);
                    let p = ui.painter().with_clip_rect(r.shrink(6.0));
                    p.text(pos + vec2(12.0, 9.0), Align2::LEFT_TOP, format!("$ {cmd}"), mono(12.0), TEXT);
                    match out {
                        None => {
                            p.text(pos + vec2(12.0, 32.0), Align2::LEFT_TOP, "running…", mono(12.0), MUTED);
                        }
                        Some((ok, tail)) => {
                            for (k, l) in tail.iter().enumerate() {
                                p.text(pos + vec2(12.0, 32.0 + k as f32 * 18.0), Align2::LEFT_TOP, l, mono(11.5), if *ok { MUTED } else { WARN_FG });
                            }
                        }
                    }
                }
                h
            }
            Item::Card(_) => 0.0,
        }
    }

    /// Draws one agent-authored card from its A2UI 1.0 messages (`createSurface`, `updateComponents`).
    fn card(&mut self, ui: &mut Ui, which: &'static str, pos: Pos2, width: f32, paint: bool) -> f32 {
        let msgs = self.replay["threads"][0]["cards"][which].clone();
        let comps: BTreeMap<String, Value> = msgs.as_array().into_iter().flatten().filter_map(|m| m["updateComponents"]["components"].as_array()).flatten().map(|c| (c["id"].as_str().unwrap_or("").to_string(), c.clone())).collect();
        let answered = if which == "permission" { self.threads[0].step > 8 } else { self.threads[0].step > 10 };
        let mut fired: Option<String> = None;
        let h = self.a2ui(ui, &comps, "root", pos, width, paint, answered, &mut fired);
        if let Some(event) = fired {
            let now = self.now;
            let t = &mut self.threads[0];
            if event.starts_with("permission_") && t.step == 8 {
                if event == "permission_deny" {
                    t.items.push(Item::Note("You denied it.".into()));
                    t.step = 12;
                    t.done = true;
                    return h;
                }
                t.items.push(Item::Note(if event == "permission_once" { "You allowed it once.".into() } else { "You allowed it for this cluster.".to_string() }));
                let cmd = self.replay["threads"][0]["command"].as_str().unwrap_or("").to_string();
                t.items.push(Item::Run { cmd: cmd.clone(), out: None });
                let at = t.items.len() - 1;
                t.step = 9;
                t.step_at = now;
                self.task_state = 1;
                self.task_note = None;
                let (tx, dest) = (self.tx.clone(), self.dest.clone());
                std::thread::spawn(move || {
                    let parts: Vec<&str> = cmd.split_whitespace().collect();
                    let out = Command::new(parts[0]).args(&parts[1..]).current_dir(&dest).stdin(Stdio::null()).output();
                    let (ok, text) = match out {
                        Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
                        Err(e) => (false, e.to_string()),
                    };
                    let tail: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).rev().take(3).map(|l| l.trim().to_string()).collect::<Vec<_>>().into_iter().rev().collect();
                    let _ = tx.send(Msg::Ran { item: at, ok, tail });
                });
            } else if event.starts_with("parity_") && t.step == 10 {
                let choice = match event.as_str() {
                    "parity_ok" => "Looks right",
                    "parity_short" => "Too short",
                    "parity_fast" => "Too fast",
                    _ => "Can't tell without a reference",
                };
                t.items.push(Item::Note(format!("You answered: {choice}.")));
                t.step = 11;
                t.step_at = now;
            }
        }
        h
    }

    #[allow(clippy::too_many_arguments)]
    fn a2ui(&mut self, ui: &mut Ui, comps: &BTreeMap<String, Value>, id: &str, pos: Pos2, width: f32, paint: bool, answered: bool, fired: &mut Option<String>) -> f32 {
        let Some(c) = comps.get(id) else { return 0.0 };
        let kids = |c: &Value| -> Vec<String> { c["children"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)).collect() };
        match c["component"].as_str().unwrap_or("") {
            "Card" => {
                let child = c["child"].as_str().unwrap_or("").to_string();
                let inner = self.a2ui(ui, comps, &child, pos + vec2(14.0, 12.0), width - 28.0, false, answered, fired);
                let r = Rect::from_min_size(pos, vec2(width, inner + 24.0));
                if paint {
                    fill(ui, r, 8.0, if id == "root" { PANEL } else { SUNKEN });
                    outline(ui, r, 8.0, if id == "root" && !answered { WARN } else { EDGE }, 1.0);
                    self.a2ui(ui, comps, &child, pos + vec2(14.0, 12.0), width - 28.0, true, answered, fired);
                }
                r.height()
            }
            "Column" => {
                let mut y = 0.0;
                for k in kids(c) {
                    y += self.a2ui(ui, comps, &k, pos + vec2(0.0, y), width, paint, answered, fired) + 8.0;
                }
                (y - 8.0).max(0.0)
            }
            "Row" => {
                let ks = kids(c);
                let buttons = ks.iter().all(|k| comps.get(k).is_some_and(|c| c["component"] == "Button"));
                if buttons {
                    let mut x = 0.0;
                    for k in &ks {
                        let label = comps.get(comps[k]["child"].as_str().unwrap_or("")).and_then(|l| l["text"].as_str()).unwrap_or("").to_string();
                        let w = measure(ui, &label, font(13.0), 400.0).x + 34.0;
                        if paint && !answered {
                            let event = comps[k]["action"]["event"]["name"].as_str().unwrap_or("").to_string();
                            if self.button(ui, &format!("a2ui:{event}"), Rect::from_min_size(pos + vec2(x, 0.0), vec2(w, 30.0)), &label, comps[k]["variant"] == "primary") {
                                *fired = Some(event);
                            }
                        } else if paint {
                            line(ui, pos + vec2(x, 7.0), &label, 12.5, EDGE);
                        }
                        x += w + 6.0;
                    }
                    30.0
                } else {
                    let w = (width - 10.0 * (ks.len() as f32 - 1.0)) / ks.len().max(1) as f32;
                    let mut h: f32 = 0.0;
                    for (i, k) in ks.iter().enumerate() {
                        h = h.max(self.a2ui(ui, comps, k, pos + vec2(i as f32 * (w + 10.0), 0.0), w, paint, answered, fired));
                    }
                    h
                }
            }
            "Text" => {
                let (size, col) = match c["variant"].as_str().unwrap_or("body") {
                    "h4" => (16.0, TEXT),
                    "caption" => (11.5, MUTED),
                    _ => (13.0, TEXT),
                };
                let s = c["text"].as_str().unwrap_or("");
                if paint { text(ui, pos, width, s, font(size), col) } else { measure(ui, s, font(size), width).y }
            }
            "Code" => {
                if paint {
                    let r = Rect::from_min_size(pos, vec2(width, 30.0));
                    fill(ui, r, 6.0, SOUP);
                    ui.painter().with_clip_rect(r).text(r.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, c["text"].as_str().unwrap_or(""), mono(12.0), MUTED);
                }
                30.0
            }
            "Divider" => {
                if paint {
                    ui.painter().line_segment([pos + vec2(0.0, 1.0), pos + vec2(width, 1.0)], Stroke::new(1.0, DIVIDER));
                }
                2.0
            }
            _ => 0.0,
        }
    }

    /// Moves the current thread's played script forward. Each step is one entry of `demo/replay.json`.
    fn advance_thread(&mut self) {
        let i = self.cur;
        let script = self.replay["threads"][i].clone();
        let (step, since) = (self.threads[i].step, self.now - self.threads[i].step_at);
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        let tool_ready = |t: &Thread| matches!(t.items.last(), Some(Item::Tool { rows: Some(_), .. }));
        let mut next: Option<Item> = None;
        match (i, step) {
            (_, 1) if since > 1.0 => {
                next = Some(Item::Tool { who: "planner".into(), adapter: "Claude adapter".into(), query: s(&script["query"]), rows: None });
            }
            (_, 2) if tool_ready(&self.threads[i]) && since > 1.4 => {
                next = Some(Item::Agent { who: "planner".into(), adapter: "Claude adapter".into(), text: s(&script["reply"]) });
            }
            (0, 3) if since > 2.4 => next = Some(Item::Peer { from: s(&script["peer"][0]["from"]), to: s(&script["peer"][0]["to"]), text: s(&script["peer"][0]["text"]) }),
            (0, 4) if since > 1.6 => next = Some(Item::Tool { who: "checker".into(), adapter: "Gemini adapter".into(), query: s(&script["peer"][1]["query"]), rows: None }),
            (0, 5) if tool_ready(&self.threads[i]) && since > 1.4 => next = Some(Item::Peer { from: s(&script["peer"][1]["from"]), to: s(&script["peer"][1]["to"]), text: s(&script["peer"][1]["text"]) }),
            (0, 6) if since > 2.4 => next = Some(Item::Peer { from: s(&script["peer"][2]["from"]), to: s(&script["peer"][2]["to"]), text: s(&script["peer"][2]["text"]) }),
            (0, 7) if since > 1.6 => {
                next = Some(Item::Card("permission"));
                self.task_state = 2;
            }
            (0, 9) if matches!(self.threads[i].items.last(), Some(Item::Run { out: Some(_), .. })) && since > 1.6 => next = Some(Item::Agent { who: "planner".into(), adapter: "Claude adapter".into(), text: s(&script["after_allow"]) }),
            (0, 11) if since > 1.0 => next = Some(Item::Agent { who: "planner".into(), adapter: "Claude adapter".into(), text: s(&script["after_parity"]) }),
            _ => {}
        }
        if step == 3 && i == 1 {
            self.threads[i].done = true;
            return;
        }
        // Step 9's follow-up is the visual card; step 11's follow-up ends the thread.
        if let Some(item) = next {
            let query = if let Item::Tool { query, .. } = &item { Some(query.clone()) } else { None };
            let now = self.now;
            let t = &mut self.threads[i];
            t.items.push(item);
            let at = t.items.len() - 1;
            match (i, step) {
                (0, 9) => {
                    t.items.push(Item::Card("visual"));
                    t.step = 10;
                }
                (0, 11) => {
                    t.step = 12;
                    t.done = true;
                }
                _ => t.step += 1,
            }
            t.step_at = now;
            if let Some(q) = query {
                self.search(i, at, &q);
            }
        }
    }
}

fn main() {
    let spec = AppSpec::new("cauldron-gui", "cauldron", Defaults::new([1280.0, 800.0], [1280.0, 800.0]));
    let result = run(spec, |ctx| App::new(ctx));
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
