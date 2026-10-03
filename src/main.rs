mod browser;
mod grab;
mod library;
mod libview;
mod meta;
mod player;
mod gui;
mod server;

use grab::{AudioMode, CookiePolicy, Options, Source};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub fn open_browser(url: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

fn help() {
    println!("[UNTITLED] TOOLS");
    println!();
    println!("  untitled-tools                      open the window");
    println!("  untitled-tools --serve [--port N] [--no-open]");
    println!("                                      host the browser UI on 127.0.0.1 only");
    println!("  untitled-tools --cli <link|saved-page.html> ... --out <folder> [options]");
    println!("        --mode mp3|original|both   (default mp3)");
    println!("        --metadata-only            save metadata, no audio");
    println!("        --no-subfolder             put files straight into --out");
    println!("        --show-browser             show the background browser window (log in / watch it)");
    println!("        --no-browser               plain HTTP instead of the background browser");
    println!("        --remember-login           keep the browser login in the app's own profile");
    println!("        --cookies ask|always|never use the browser's login session for downloads (default ask)");
    println!("        --no-json --no-csv --no-playlist --no-covers --no-tags --no-artist");
    println!("                                   leave that part out (default: everything is saved)");
    println!("        --preview                  list what would be saved, save nothing");
    println!("        --allow-nondownloadable    I own this / have permission");
    println!("  env UNTITLED_TOKEN                  optional access token (memory only)");
}

fn serve_cli(args: &[String]) {
    let mut port: u16 = 8787;
    let mut open = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                i += 1;
                port = args.get(i).and_then(|p| p.parse().ok()).unwrap_or(8787);
            }
            "--no-open" => open = false,
            _ => {}
        }
        i += 1;
    }
    if open {
        let url = format!("http://127.0.0.1:{}/", port);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            open_browser(&url);
        });
    }
    let sink: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|m: String| println!("{}", m));
    if let Err(e) = server::serve(port, Arc::new(AtomicBool::new(false)), sink) {
        eprintln!("Server error: {}", e);
        std::process::exit(1);
    }
}

fn cli(args: &[String]) {
    let mut sources: Vec<Source> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut opts = Options::default();
    let mut preview = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out = args.get(i).map(PathBuf::from);
            }
            "--mode" => {
                i += 1;
                opts.mode = AudioMode::parse(args.get(i).map(|s| s.as_str()).unwrap_or("mp3"));
            }
            "--metadata-only" => opts.metadata_only = true,
            "--no-subfolder" => opts.subfolder = false,
            "--preview" => preview = true,
            "--no-browser" => opts.use_browser = false,
            "--no-json" => {
                opts.toggles.track_json = false;
                opts.toggles.project_json = false;
            }
            "--no-csv" => opts.toggles.csv = false,
            "--no-playlist" => opts.toggles.playlist = false,
            "--no-covers" => opts.toggles.covers = false,
            "--no-tags" => opts.toggles.embed_tags = false,
            "--no-artist" => opts.toggles.artist = false,
            "--show-browser" => browser::set_show_window(true),
            "--remember-login" => opts.remember_login = true,
            "--cookies" => {
                i += 1;
                opts.cookies = CookiePolicy::parse(args.get(i).map(|s| s.as_str()).unwrap_or("ask"));
            }
            "--allow-nondownloadable" => opts.allow_nondownloadable = true,
            other => {
                if other.starts_with("http") {
                    sources.push(Source::Url(other.to_string()));
                } else {
                    sources.push(Source::File(PathBuf::from(other)));
                }
            }
        }
        i += 1;
    }
    opts.ask = Some(Arc::new(|msg: &str| {
        println!("{} [y/N]", msg);
        let mut a = String::new();
        let _ = std::io::stdin().read_line(&mut a);
        matches!(a.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }));
    opts.token_override = std::env::var("UNTITLED_TOKEN").ok().filter(|t| !t.trim().is_empty());
    if sources.is_empty() || (out.is_none() && !preview) {
        help();
        std::process::exit(2);
    }
    let logf = |m: String| println!("{}", m);
    let cancel = AtomicBool::new(false);
    let mut failed = false;
    for s in &sources {
        let r = if preview {
            grab::preview(s, &opts, &logf)
        } else {
            grab::run(s, out.as_ref().unwrap(), &opts, &logf, &cancel)
        };
        if let Err(e) = r {
            eprintln!("ERROR: {}", e);
            failed = true;
        }
    }
    if failed {
        std::process::exit(1);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        let res = gui::launch();
        browser::close();
        if let Err(e) = res {
            eprintln!("Could not open the window: {}", e);
            eprintln!("Try the browser version instead:  untitled-tools --serve");
            std::process::exit(1);
        }
        return;
    }
    match args[0].as_str() {
        "--serve" => serve_cli(&args[1..]),
        "--cli" => cli(&args[1..]),
        _ => help(),
    }
}
