//! The Library tab: browse downloaded projects the way untitled.stream shows them.

use crate::library::{self, LibProject};
use crate::meta;
use crate::player::{PlayItem, Player};
use eframe::egui::{self, load::SizedTexture, Color32, RichText, Rounding, Vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

const GRAY: Color32 = Color32::from_gray(120);
const DARK: Color32 = Color32::from_rgb(20, 20, 20);

enum Act {
    PlayFrom(usize),
    PlayAll(bool),
    Props(usize),
    Reveal(usize),
    OpenFolder,
}

struct Row {
    title: String,
    sub: String,
    dur: String,
    path: PathBuf,
}

pub struct LibraryUi {
    root: Option<PathBuf>,
    projects: Vec<LibProject>,
    pending: Arc<Mutex<Option<Vec<LibProject>>>>,
    scanning: Arc<AtomicBool>,
    selected: Option<usize>,
    textures: HashMap<String, egui::TextureHandle>,
    pub player: Player,
    props: Option<(usize, usize)>,
    filter: String,
    status: String,
    auto_tried: bool,
    stale: bool,
}

fn fmt_total(s: f64) -> String {
    let t = s.round() as u64;
    if t >= 3600 {
        format!("{}h {}m", t / 3600, (t % 3600) / 60)
    } else {
        format!("{}m {:02}s", t / 60, t % 60)
    }
}

fn fmt_clock(s: f64) -> String {
    let t = if s.is_finite() && s > 0.0 { s.round() as u64 } else { 0 };
    format!("{:02}:{:02}", t / 60, t % 60)
}

fn fmt_size(b: u64) -> String {
    if b >= 1024 * 1024 {
        format!("{:.1} MB", b as f64 / 1048576.0)
    } else {
        format!("{} KB", b / 1024)
    }
}

fn time_ago(t: Option<SystemTime>) -> String {
    let t = match t {
        Some(t) => t,
        None => return String::new(),
    };
    let secs = SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (n, unit) = if secs < 60 {
        return "just now".to_string();
    } else if secs < 3600 {
        (secs / 60, "minute")
    } else if secs < 86400 {
        (secs / 3600, "hour")
    } else if secs < 86400 * 30 {
        (secs / 86400, "day")
    } else if secs < 86400 * 365 {
        (secs / (86400 * 30), "month")
    } else {
        (secs / (86400 * 365), "year")
    };
    format!("{} {}{} ago", n, unit, if n == 1 { "" } else { "s" })
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut o: String = s.chars().take(n.saturating_sub(3)).collect();
        o.push_str("...");
        o
    }
}

fn open_path(p: &Path) {
    #[cfg(windows)]
    {
        let _ = Command::new("explorer").arg(p).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(p).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(p).spawn();
    }
}

fn reveal(p: &Path) {
    #[cfg(windows)]
    {
        let _ = Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(d) = p.parent() {
            open_path(d);
        }
    }
}

fn prow(ui: &mut egui::Ui, k: &str, v: String) {
    ui.label(RichText::new(k).color(GRAY));
    ui.label(v);
    ui.end_row();
}

impl LibraryUi {
    pub fn new() -> LibraryUi {
        LibraryUi {
            root: library::load_root(),
            projects: Vec::new(),
            pending: Arc::new(Mutex::new(None)),
            scanning: Arc::new(AtomicBool::new(false)),
            selected: None,
            textures: HashMap::new(),
            player: Player::new(),
            props: None,
            filter: String::new(),
            status: String::new(),
            auto_tried: false,
            stale: false,
        }
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    pub fn tick(&mut self) {
        self.player.tick();
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        let root = match &self.root {
            Some(r) => r.clone(),
            None => return,
        };
        if self.scanning.swap(true, Ordering::SeqCst) {
            return;
        }
        self.status = "Scanning...".to_string();
        let pending = self.pending.clone();
        let scanning = self.scanning.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let v = library::scan(&root);
            if let Ok(mut g) = pending.lock() {
                *g = Some(v);
            }
            scanning.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    fn texture_for(&mut self, ctx: &egui::Context, idx: usize) -> egui::TextureId {
        let key = self.projects[idx].dir.to_string_lossy().to_string();
        if let Some(t) = self.textures.get(&key) {
            return t.id();
        }
        let (w, h, rgba) = match &self.projects[idx].cover {
            Some(c) => c.clone(),
            None => meta::gradient_image(&self.projects[idx].gradient_id, 256),
        };
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let tex = ctx.load_texture(format!("cover-{}", key), img, egui::TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert(key, tex);
        id
    }

    fn play_project(&mut self, pi: usize, start: usize, shuffle: bool) {
        let p = match self.projects.get(pi) {
            Some(p) => p,
            None => return,
        };
        let key = p.dir.to_string_lossy().to_string();
        let items: Vec<PlayItem> = p
            .tracks
            .iter()
            .map(|t| PlayItem {
                path: t.path.clone(),
                title: t.title.clone(),
                album: p.title.clone(),
                duration: t.info.duration_secs,
                cover_key: key.clone(),
            })
            .collect();
        self.player.shuffle = shuffle;
        let start = if shuffle { 0 } else { start };
        self.player.play_queue(items, start);
    }

    pub fn ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, suggested: Option<PathBuf>) {
        if let Ok(mut g) = self.pending.lock() {
            if let Some(v) = g.take() {
                self.projects = v;
                self.textures.clear();
                if let Some(s) = self.selected {
                    if s >= self.projects.len() {
                        self.selected = None;
                    }
                }
                self.status = format!("{} project(s)", self.projects.len());
            }
        }
        if !self.auto_tried {
            self.auto_tried = true;
            if self.root.is_none() {
                self.root = suggested;
            }
            self.stale = true;
        }
        if self.stale {
            self.stale = false;
            self.start_scan(ctx);
        }

        ui.horizontal(|ui| {
            if self.selected.is_some() && ui.button("<  Library").clicked() {
                self.selected = None;
            }
            ui.label(RichText::new("Library").size(22.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Choose folder...").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        library::save_root(&p);
                        self.root = Some(p);
                        self.selected = None;
                        self.start_scan(ctx);
                    }
                }
                if ui.button("Refresh").clicked() {
                    self.start_scan(ctx);
                }
                if let Some(r) = &self.root {
                    ui.label(RichText::new(clip(&r.display().to_string(), 60)).color(GRAY));
                }
            });
        });
        ui.add_space(8.0);

        match self.selected {
            Some(i) if i < self.projects.len() => self.project_page(ctx, ui, i),
            _ => self.grid(ctx, ui),
        }
        self.props_window(ctx);
    }

    fn grid(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        if self.root.is_none() {
            ui.label("Choose the folder your downloads are in (the one that holds one folder per project).");
            return;
        }
        if self.projects.is_empty() {
            let busy = self.scanning.load(Ordering::SeqCst);
            ui.label(if busy { "Scanning..." } else { "No audio found in this folder yet." });
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(220.0));
            ui.label(RichText::new(self.status.clone()).color(GRAY));
        });
        ui.add_space(6.0);
        let q = self.filter.to_lowercase();
        let visible: Vec<usize> = (0..self.projects.len())
            .filter(|&i| {
                let p = &self.projects[i];
                q.is_empty()
                    || p.title.to_lowercase().contains(&q)
                    || p.artist.as_deref().unwrap_or("").to_lowercase().contains(&q)
                    || p.tracks.iter().any(|t| t.title.to_lowercase().contains(&q))
            })
            .collect();
        let mut open: Option<usize> = None;
        let size = 180.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for &i in &visible {
                    let tex = self.texture_for(ctx, i);
                    let (title, sub) = {
                        let p = &self.projects[i];
                        let who = p.artist.clone().unwrap_or_else(|| "Unknown artist".to_string());
                        (
                            clip(&p.title, 22),
                            clip(&format!("{} - {} tracks", who, p.tracks.len()), 28),
                        )
                    };
                    ui.vertical(|ui| {
                        ui.set_width(size);
                        let r = ui.add(
                            egui::Image::new(SizedTexture::new(tex, Vec2::splat(size)))
                                .rounding(Rounding::same(14.0))
                                .sense(egui::Sense::click()),
                        );
                        if r.clicked() {
                            open = Some(i);
                        }
                        let _ = r.on_hover_cursor(egui::CursorIcon::PointingHand);
                        ui.label(RichText::new(title).strong());
                        ui.label(RichText::new(sub).small().color(GRAY));
                        ui.add_space(14.0);
                    });
                    ui.add_space(10.0);
                }
            });
        });
        if open.is_some() {
            self.selected = open;
        }
    }

    fn project_page(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, pi: usize) {
        let tex = self.texture_for(ctx, pi);
        let playing = self.player.current_path();
        let (title, sub, dir, rows, details) = {
            let p = &self.projects[pi];
            let who = p.artist.clone().unwrap_or_else(|| "Unknown artist".to_string());
            let sub = format!(
                "{} - {} track{} - {}",
                who,
                p.tracks.len(),
                if p.tracks.len() == 1 { "" } else { "s" },
                fmt_total(p.total_secs)
            );
            let rows: Vec<Row> = p
                .tracks
                .iter()
                .map(|t| Row {
                    title: t.title.clone(),
                    sub: time_ago(t.modified),
                    dur: fmt_clock(t.info.duration_secs),
                    path: t.path.clone(),
                })
                .collect();
            let details = vec![
                ("Folder", p.dir.display().to_string()),
                ("Songs", p.tracks.len().to_string()),
                ("Total length", fmt_total(p.total_secs)),
                ("Total size", fmt_size(p.total_bytes)),
                ("Cover", p.cover_source.clone()),
                (
                    "_project.json",
                    if p.has_json { "found".to_string() } else { "not needed (everything is read from the files)".to_string() },
                ),
            ];
            (p.title.clone(), sub, p.dir.clone(), rows, details)
        };
        let mut act: Option<Act> = None;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.add_space(8.0);
                ui.add(
                    egui::Image::new(SizedTexture::new(tex, Vec2::splat(300.0)))
                        .rounding(Rounding::same(14.0)),
                );
                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().min(560.0));
                    ui.label(RichText::new(&title).size(30.0).strong());
                    ui.label(RichText::new(&sub).color(GRAY));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let play = egui::Button::new(RichText::new("   Play   ").color(Color32::WHITE).strong())
                            .fill(DARK)
                            .rounding(Rounding::same(12.0));
                        if ui.add(play).clicked() {
                            act = Some(Act::PlayAll(false));
                        }
                        if ui.button("Shuffle").clicked() {
                            act = Some(Act::PlayAll(true));
                        }
                    });
                    ui.add_space(8.0);
                    if ui
                        .add_sized([ui.available_width(), 38.0], egui::Button::new("Open folder"))
                        .clicked()
                    {
                        act = Some(Act::OpenFolder);
                    }
                    ui.add_space(10.0);
                    for (ti, row) in rows.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [26.0, 40.0],
                                egui::Label::new(RichText::new(format!("{}", ti + 1)).color(GRAY)),
                            );
                            ui.vertical(|ui| {
                                let is_cur = playing.as_ref() == Some(&row.path);
                                let mut t = RichText::new(clip(&row.title, 44)).strong();
                                if is_cur {
                                    t = t.color(Color32::from_rgb(97, 40, 210));
                                }
                                let r = ui.add(egui::Label::new(t).sense(egui::Sense::click()));
                                if r.clicked() {
                                    act = Some(Act::PlayFrom(ti));
                                }
                                let _ = r.on_hover_cursor(egui::CursorIcon::PointingHand);
                                ui.label(RichText::new(&row.sub).small().color(GRAY));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.menu_button("...", |ui| {
                                    if ui.button("Play").clicked() {
                                        act = Some(Act::PlayFrom(ti));
                                        ui.close_menu();
                                    }
                                    if ui.button("Properties").clicked() {
                                        act = Some(Act::Props(ti));
                                        ui.close_menu();
                                    }
                                    if ui.button("Show in folder").clicked() {
                                        act = Some(Act::Reveal(ti));
                                        ui.close_menu();
                                    }
                                });
                                ui.label(RichText::new(&row.dur).color(GRAY));
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
            });
            ui.add_space(16.0);
            ui.collapsing("Details", |ui| {
                egui::Grid::new("proj_details").num_columns(2).striped(true).show(ui, |ui| {
                    for (k, v) in &details {
                        prow(ui, k, v.clone());
                    }
                });
            });
        });

        match act {
            Some(Act::PlayFrom(i)) => self.play_project(pi, i, false),
            Some(Act::PlayAll(s)) => self.play_project(pi, 0, s),
            Some(Act::Props(i)) => self.props = Some((pi, i)),
            Some(Act::Reveal(i)) => {
                if let Some(r) = rows.get(i) {
                    reveal(&r.path);
                }
            }
            Some(Act::OpenFolder) => open_path(&dir),
            None => {}
        }
    }

    fn props_window(&mut self, ctx: &egui::Context) {
        let (pp, tt) = match self.props {
            Some(x) => x,
            None => return,
        };
        let t = match self.projects.get(pp).and_then(|p| p.tracks.get(tt)) {
            Some(t) => t.clone(),
            None => {
                self.props = None;
                return;
            }
        };
        let mut open = true;
        let name = t.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        egui::Window::new(format!("Properties - {}", clip(&t.title, 40)))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .show(ctx, |ui| {
                let i = &t.info;
                egui::Grid::new("track_props").num_columns(2).striped(true).show(ui, |ui| {
                    prow(ui, "File", name.clone());
                    prow(ui, "Location", t.path.parent().map(|d| d.display().to_string()).unwrap_or_default());
                    prow(ui, "Format", i.ext.to_uppercase());
                    prow(ui, "Size", fmt_size(i.size_bytes));
                    prow(ui, "Length", fmt_clock(i.duration_secs));
                    prow(ui, "Bitrate", i.bitrate_kbps.map(|b| format!("{} kbps", b)).unwrap_or_else(|| "-".to_string()));
                    prow(ui, "Sample rate", i.sample_rate.map(|b| format!("{} Hz", b)).unwrap_or_else(|| "-".to_string()));
                    prow(ui, "Channels", i.channels.map(|b| b.to_string()).unwrap_or_else(|| "-".to_string()));
                    prow(ui, "Bit depth", i.bit_depth.map(|b| b.to_string()).unwrap_or_else(|| "-".to_string()));
                    prow(ui, "Title (tag)", i.title.clone().unwrap_or_else(|| "-".to_string()));
                    prow(ui, "Artist (tag)", i.artist.clone().unwrap_or_else(|| "-".to_string()));
                    prow(ui, "Album (tag)", i.album.clone().unwrap_or_else(|| "-".to_string()));
                    prow(ui, "Track number", i.track_no.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string()));
                    prow(ui, "BPM", i.bpm.clone().unwrap_or_else(|| "-".to_string()));
                    prow(ui, "Key", i.key.clone().unwrap_or_else(|| "-".to_string()));
                    prow(ui, "Cover inside file", if i.has_cover { "yes".to_string() } else { "no".to_string() });
                    if !t.alt_paths.is_empty() {
                        let others: Vec<String> = t
                            .alt_paths
                            .iter()
                            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                            .collect();
                        prow(ui, "Also saved as", others.join(", "));
                    }
                });
            });
        if !open {
            self.props = None;
        }
    }

    /// The dark floating bar at the bottom (call before the central panel).
    pub fn player_bar(&mut self, ctx: &egui::Context) {
        if !self.player.active() {
            return;
        }
        let item = match self.player.current() {
            Some(i) => i.clone(),
            None => return,
        };
        let tex = self.textures.get(&item.cover_key).map(|t| t.id());
        let mut pos = self.player.position();
        let dur = if item.duration > 0.0 { item.duration } else { pos.max(1.0) };
        let playing = self.player.is_playing();
        let mut seek_to: Option<f64> = None;
        let mut vol = self.player.volume;
        let err = self.player.error.clone();

        egui::TopBottomPanel::bottom("player_bar")
            .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(0.0, 10.0)))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    egui::Frame::none()
                        .fill(DARK)
                        .rounding(Rounding::same(28.0))
                        .inner_margin(egui::Margin::symmetric(16.0, 8.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if let Some(t) = tex {
                                    ui.add(
                                        egui::Image::new(SizedTexture::new(t, Vec2::splat(36.0)))
                                            .rounding(Rounding::same(18.0)),
                                    );
                                }
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(clip(&item.title, 24)).color(Color32::WHITE).strong());
                                    ui.label(RichText::new(clip(&item.album, 24)).small().color(Color32::from_gray(170)));
                                });
                                ui.add_space(8.0);
                                ui.spacing_mut().slider_width = 150.0;
                                let r = ui.add(egui::Slider::new(&mut pos, 0.0..=dur).show_value(false));
                                if r.drag_stopped() {
                                    seek_to = Some(pos);
                                } else if r.changed() && !r.dragged() {
                                    seek_to = Some(pos);
                                }
                                ui.label(
                                    RichText::new(format!("{} / {}", fmt_clock(pos), fmt_clock(dur)))
                                        .monospace()
                                        .small()
                                        .color(Color32::WHITE),
                                );
                                if ui.small_button("Prev").clicked() {
                                    self.player.prev();
                                }
                                if ui.small_button(if playing { "Pause" } else { "Play" }).clicked() {
                                    self.player.toggle_pause();
                                }
                                if ui.small_button("Next").clicked() {
                                    self.player.next();
                                }
                                ui.checkbox(&mut self.player.shuffle, RichText::new("Shuffle").small().color(Color32::WHITE));
                                ui.checkbox(&mut self.player.repeat, RichText::new("Repeat").small().color(Color32::WHITE));
                                ui.spacing_mut().slider_width = 70.0;
                                if ui.add(egui::Slider::new(&mut vol, 0.0..=1.0).show_value(false)).changed() {
                                    self.player.set_volume(vol);
                                }
                            });
                            if let Some(e) = &err {
                                ui.label(RichText::new(e).small().color(Color32::from_rgb(255, 140, 140)));
                            }
                        });
                });
            });
        if let Some(s) = seek_to {
            self.player.seek(s);
        }
    }
}
