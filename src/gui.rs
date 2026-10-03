//! Native window (egui).

use crate::grab::{self, AudioMode, CookiePolicy, Options, Source, Toggles};
use crate::libview::LibraryUi;
use crate::meta;
use crate::{open_browser, server};
use eframe::egui::{self, Color32, Rounding};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Grab,
    Library,
    Tools,
    Server,
    About,
}

pub struct App {
    tab: Tab,
    urls: String,
    out_dir: Option<PathBuf>,
    mode: usize,
    subfolder: bool,
    metadata_only: bool,
    allow_nd: bool,
    toggles: Toggles,
    lib: LibraryUi,
    use_browser: bool,
    show_window: bool,
    remember_login: bool,
    cookie_mode: usize,
    need_window: Arc<AtomicBool>,
    token: String,
    log: Arc<Mutex<Vec<String>>>,
    busy: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    tools_dir: Option<PathBuf>,
    port: String,
    srv_stop: Option<Arc<AtomicBool>>,
}

impl Default for App {
    fn default() -> Self {
        App {
            tab: Tab::Grab,
            urls: String::new(),
            out_dir: None,
            mode: 0,
            subfolder: true,
            metadata_only: false,
            allow_nd: false,
            toggles: Toggles::default(),
            lib: LibraryUi::new(),
            use_browser: true,
            show_window: false,
            remember_login: false,
            cookie_mode: 0,
            need_window: Arc::new(AtomicBool::new(false)),
            token: String::new(),
            log: Arc::new(Mutex::new(vec!["Ready. Paste a project link, pick a folder, press Download.".to_string()])),
            busy: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            tools_dir: None,
            port: "8787".to_string(),
            srv_stop: None,
        }
    }
}

pub fn launch() -> Result<(), String> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([980.0, 700.0])
        .with_min_inner_size([640.0, 460.0]);
    if let Some((w, h, rgba)) = meta::decode_image(include_bytes!("assets/icon.png"), 256) {
        viewport = viewport.with_icon(egui::IconData { rgba, width: w, height: h });
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "[UNTITLED] TOOLS",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(untitled_visuals());
            Ok(Box::new(App::default()))
        }),
    )
    .map_err(|e| e.to_string())
}

/// Light, rounded look in the spirit of untitled.stream.
fn untitled_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    v.panel_fill = Color32::WHITE;
    v.window_fill = Color32::WHITE;
    v.extreme_bg_color = Color32::from_rgb(246, 246, 246);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(243, 243, 243);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(232, 232, 232);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(220, 220, 220);
    let r = Rounding::same(10.0);
    v.widgets.noninteractive.rounding = r;
    v.widgets.inactive.rounding = r;
    v.widgets.hovered.rounding = r;
    v.widgets.active.rounding = r;
    v.widgets.open.rounding = r;
    v
}

impl App {
    fn say(&self, m: &str) {
        if let Ok(mut g) = self.log.lock() {
            g.push(m.to_string());
        }
    }

    fn options(&self) -> Options {
        Options {
            mode: match self.mode {
                1 => AudioMode::Original,
                2 => AudioMode::Both,
                _ => AudioMode::Mp3,
            },
            subfolder: self.subfolder,
            metadata_only: self.metadata_only,
            allow_nondownloadable: self.allow_nd,
            toggles: self.toggles,
            use_browser: self.use_browser,
            remember_login: self.remember_login,
            cookies: match self.cookie_mode {
                1 => CookiePolicy::Always,
                2 => CookiePolicy::Never,
                _ => CookiePolicy::Ask,
            },
            ask: Some(Arc::new(|msg: &str| {
                rfd::MessageDialog::new()
                    .set_title("[UNTITLED] TOOLS")
                    .set_description(msg)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes
            })),
            token_override: if self.token.trim().is_empty() {
                None
            } else {
                Some(self.token.trim().to_string())
            },
        }
    }

    fn sources(&self) -> Vec<Source> {
        self.urls
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(|l| Source::Url(l.to_string()))
            .collect()
    }

    fn spawn(&mut self, ctx: &egui::Context, sources: Vec<Source>, preview: bool) {
        if self.busy.load(Ordering::SeqCst) {
            return;
        }
        if sources.is_empty() {
            self.say("Paste at least one untitled.stream project link first.");
            return;
        }
        let dir = self.out_dir.clone();
        if !preview && dir.is_none() {
            self.say("Choose an output folder first.");
            return;
        }
        let opts = self.options();
        let log = self.log.clone();
        let busy = self.busy.clone();
        let cancel = self.cancel.clone();
        let need_window = self.need_window.clone();
        need_window.store(false, Ordering::SeqCst);
        crate::browser::set_show_window(self.show_window);
        crate::browser::cancel(false);
        let ctx = ctx.clone();
        busy.store(true, Ordering::SeqCst);
        cancel.store(false, Ordering::SeqCst);
        std::thread::spawn(move || {
            let logf = |m: String| {
                if let Ok(mut g) = log.lock() {
                    g.push(m);
                }
                ctx.request_repaint();
            };
            for src in sources.iter() {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let r = if preview {
                    grab::preview(src, &opts, &logf)
                } else {
                    match dir.as_ref() {
                        Some(d) => grab::run(src, d, &opts, &logf, &cancel),
                        None => Err("no folder".to_string()),
                    }
                };
                if let Err(e) = r {
                    if e.starts_with("NEEDS_WINDOW") {
                        need_window.store(true, Ordering::SeqCst);
                    }
                    logf(format!("ERROR: {}", e));
                }
            }
            logf("Done.".to_string());
            busy.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    fn ui_log(&self, ui: &mut egui::Ui) {
        ui.separator();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Ok(g) = self.log.lock() {
                    for l in g.iter() {
                        ui.label(egui::RichText::new(l.as_str()).monospace());
                    }
                }
            });
    }

    fn ui_grab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.label("Paste one or more untitled.stream project links (one per line):");
        ui.add(
            egui::TextEdit::multiline(&mut self.urls)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("https://untitled.stream/library/project/..."),
        );
        ui.horizontal(|ui| {
            if ui.button("Choose folder...").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.out_dir = Some(p);
                }
            }
            match &self.out_dir {
                Some(p) => ui.monospace(p.display().to_string()),
                None => ui.weak("no folder selected"),
            };
        });
        ui.horizontal(|ui| {
            egui::ComboBox::from_label("Audio")
                .selected_text(["MP3 only", "Original file only", "MP3 + original"][self.mode.min(2)])
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.mode, 0usize, "MP3 only");
                    ui.selectable_value(&mut self.mode, 1usize, "Original file only");
                    ui.selectable_value(&mut self.mode, 2usize, "MP3 + original");
                });
            ui.checkbox(&mut self.subfolder, "Folder per project");
            ui.checkbox(&mut self.metadata_only, "Metadata only");
        });
        ui.checkbox(
            &mut self.allow_nd,
            "I own this / have permission (include tracks the owner marked not downloadable)",
        );
        ui.collapsing("What to save", |ui| {
            ui.checkbox(&mut self.toggles.audio, "Audio files");
            ui.checkbox(
                &mut self.toggles.embed_tags,
                "Write the info into the MP3 itself (title, artist, album, BPM, key, cover)",
            );
            ui.checkbox(
                &mut self.toggles.covers,
                "Cover images (project cover, and song covers if the site has them)",
            );
            ui.checkbox(
                &mut self.toggles.artist,
                "Artist name (the public artist field - never the account username)",
            );
            ui.checkbox(&mut self.toggles.track_json, "One .json per song");
            ui.checkbox(&mut self.toggles.project_json, "_project.json");
            ui.checkbox(&mut self.toggles.csv, "_tracks.csv");
            ui.checkbox(&mut self.toggles.playlist, "_playlist.m3u8");
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.use_browser, "Use the background browser");
            ui.checkbox(&mut self.show_window, "Show browser window");
            ui.checkbox(&mut self.remember_login, "Remember my login between runs");
        });
        ui.horizontal(|ui| {
            egui::ComboBox::from_label("Browser login for downloads")
                .selected_text(["Ask me if the server refuses", "Always use it (this run only)", "Never"][self.cookie_mode.min(2)])
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.cookie_mode, 0usize, "Ask me if the server refuses");
                    ui.selectable_value(&mut self.cookie_mode, 1usize, "Always use it (this run only)");
                    ui.selectable_value(&mut self.cookie_mode, 2usize, "Never");
                });
            if ui.add_enabled(!self.busy.load(Ordering::SeqCst), egui::Button::new("Open browser window now")).clicked() {
                self.show_window = true;
                let remember = self.remember_login;
                let log = self.log.clone();
                let ctx2 = ctx.clone();
                std::thread::spawn(move || {
                    let logf = |m: String| {
                        if let Ok(mut g) = log.lock() {
                            g.push(m);
                        }
                        ctx2.request_repaint();
                    };
                    if let Err(e) = crate::browser::open_window(remember, &logf) {
                        logf(format!("ERROR: {}", e));
                    }
                });
            }
            if ui.button("Close browser").clicked() {
                std::thread::spawn(crate::browser::close);
            }
        });
        if self.need_window.load(Ordering::SeqCst) && !self.busy.load(Ordering::SeqCst) {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::YELLOW, "The site needs a login or a human check.");
                if ui.button("Open the browser window and retry").clicked() {
                    self.show_window = true;
                    self.need_window.store(false, Ordering::SeqCst);
                    let s = self.sources();
                    self.spawn(ctx, s, false);
                }
            });
        }
        ui.collapsing("Advanced", |ui| {
            ui.label("Access token (optional, kept in memory only, never saved or logged):");
            ui.add(egui::TextEdit::singleline(&mut self.token).password(true).desired_width(360.0));
        });
        let busy = self.busy.load(Ordering::SeqCst);
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Download")).clicked() {
                let s = self.sources();
                self.spawn(ctx, s, false);
            }
            if ui.add_enabled(!busy, egui::Button::new("Preview (saves nothing)")).clicked() {
                let s = self.sources();
                self.spawn(ctx, s, true);
            }
            if ui.add_enabled(!busy, egui::Button::new("Open saved page...")).clicked() {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter("Saved page", &["html", "htm", "txt"])
                    .pick_file()
                {
                    self.spawn(ctx, vec![Source::File(f)], false);
                }
            }
            if ui.add_enabled(busy, egui::Button::new("Cancel")).clicked() {
                self.cancel.store(true, Ordering::SeqCst);
                crate::browser::cancel(true);
            }
        });
        self.ui_log(ui);
    }

    fn ui_tools(&mut self, ui: &mut egui::Ui) {
        ui.label("Local helpers for a folder this app made (no network):");
        ui.horizontal(|ui| {
            if ui.button("Choose folder...").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.tools_dir = Some(p);
                }
            }
            match &self.tools_dir {
                Some(p) => ui.monospace(p.display().to_string()),
                None => ui.weak("no folder selected"),
            };
        });
        let log = self.log.clone();
        let logf = move |m: String| {
            if let Ok(mut g) = log.lock() {
                g.push(m);
            }
        };
        ui.horizontal(|ui| {
            if ui.button("Build playlist.m3u8").clicked() {
                match &self.tools_dir {
                    Some(d) => match grab::build_playlist(d) {
                        Ok(n) => logf(format!("playlist.m3u8 written ({} tracks).", n)),
                        Err(e) => logf(format!("ERROR: {}", e)),
                    },
                    None => logf("Choose a folder first.".to_string()),
                }
            }
            if ui.button("Verify download").clicked() {
                match &self.tools_dir {
                    Some(d) => {
                        if let Err(e) = grab::verify_folder(d, &logf) {
                            logf(format!("ERROR: {}", e));
                        }
                    }
                    None => logf("Choose a folder first.".to_string()),
                }
            }
        });
        self.ui_log(ui);
    }

    fn ui_server(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.label("Host the browser version of this tool on THIS computer only (127.0.0.1).");
        ui.horizontal(|ui| {
            ui.label("Port:");
            ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(70.0));
            let port: u16 = self.port.trim().parse().unwrap_or(8787);
            if self.srv_stop.is_none() {
                if ui.button("Start server").clicked() {
                    let stop = Arc::new(AtomicBool::new(false));
                    self.srv_stop = Some(stop.clone());
                    let log = self.log.clone();
                    let ctx2 = ctx.clone();
                    std::thread::spawn(move || {
                        let sink: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |m: String| {
                            if let Ok(mut g) = log.lock() {
                                g.push(m);
                            }
                            ctx2.request_repaint();
                        });
                        if let Err(e) = server::serve(port, stop, sink.clone()) {
                            (sink)(format!("Server error: {}", e));
                        }
                    });
                }
            } else {
                if ui.button("Stop server").clicked() {
                    if let Some(s) = self.srv_stop.take() {
                        s.store(true, Ordering::SeqCst);
                    }
                }
                if ui.button("Open in browser").clicked() {
                    open_browser(&format!("http://127.0.0.1:{}/", port));
                }
            }
        });
        self.ui_log(ui);
    }

    fn ui_about(&self, ui: &mut egui::Ui) {
        ui.heading("What gets saved");
        ui.label("Audio files, named \"NN - Title.ext\", with the info written into the MP3 itself (title, artist, album, BPM, key, cover).");
        ui.label("Optional extras you can switch off under \"What to save\": a .json per song, _project.json, _tracks.csv, _playlist.m3u8 and cover images.");
        ui.label("Per track: title, length (read from the file), BPM, key, audio format info, created date, and the owner's download flag.");
        ui.add_space(8.0);
        ui.heading("What is never saved");
        ui.label("Account usernames, e-mail addresses, profile pictures, account IDs, session tokens, or any links.");
        ui.label("The artist name shown on the project is saved (you can switch that off); it is not the account username.");
        ui.label("The page is read in memory; only the fields above are ever picked out of it.");
        ui.add_space(8.0);
        ui.heading("Rules it follows");
        ui.label("Only talks to untitled.stream. Skips password-protected and paid projects. Skips tracks the owner marked not downloadable unless you tick the permission box.");
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("[UNTITLED] TOOLS");
                ui.separator();
                let before = self.tab;
                ui.selectable_value(&mut self.tab, Tab::Grab, "Grabber");
                ui.selectable_value(&mut self.tab, Tab::Library, "Library");
                if before != Tab::Library && self.tab == Tab::Library {
                    self.lib.mark_stale();
                }
                ui.selectable_value(&mut self.tab, Tab::Tools, "Tools");
                ui.selectable_value(&mut self.tab, Tab::Server, "Server");
                ui.selectable_value(&mut self.tab, Tab::About, "Privacy");
            });
        });
        self.lib.tick();
        self.lib.player_bar(ctx);
        let suggested = self.out_dir.clone();
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Grab => self.ui_grab(ctx, ui),
            Tab::Library => self.lib.ui(ctx, ui, suggested),
            Tab::Tools => self.ui_tools(ui),
            Tab::Server => self.ui_server(ctx, ui),
            Tab::About => self.ui_about(ui),
        });
        if self.busy.load(Ordering::SeqCst) || self.lib.player.is_playing() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}
