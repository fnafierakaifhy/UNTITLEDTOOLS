//! Background browser: drives an installed Chromium-based browser (Edge ships with
//! Windows 10/11; Chrome and Brave also work) over the DevTools protocol.
//!
//! * Default: a brand-new TEMPORARY profile, deleted when the run ends. Your real
//!   browser profile, cookies and passwords are never read.
//! * Optional "remember login": the app's OWN profile folder is kept between runs.
//! * Hidden by default. If the site needs a login or a human check, fetch_page returns
//!   an error starting with "NEEDS_WINDOW" and the app offers to show the window.
//! * Cookies are only handed to the downloader (memory only) when the person allows it.

use serde_json::{json, Value};
use std::fs;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

use crate::grab::find_project;

static LIVE: Mutex<Option<Live>> = Mutex::new(None);
static SHOW: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn set_show_window(b: bool) {
    SHOW.store(b, Ordering::SeqCst);
}

pub fn cancel(b: bool) {
    CANCEL.store(b, Ordering::SeqCst);
}

pub fn is_running() -> bool {
    LIVE.lock().map(|g| g.is_some()).unwrap_or(false)
}

pub fn user_agent() -> Option<String> {
    LIVE.lock().ok().and_then(|g| g.as_ref().map(|l| l.ua.clone()))
}

/// Cookie header for the media host. Memory only: never logged, never written.
pub fn cookie_header() -> Option<String> {
    let mut g = LIVE.lock().ok()?;
    let live = g.as_mut()?;
    let sid = live.sid.clone();
    let r = live
        .cdp
        .call(
            Some(sid.as_str()),
            "Network.getCookies",
            json!({ "urls": ["https://sb.untitled.stream/"] }),
            Duration::from_secs(10),
        )
        .ok()?;
    let parts: Vec<String> = r["cookies"]
        .as_array()?
        .iter()
        .filter_map(|c| match (c["name"].as_str(), c["value"].as_str()) {
            (Some(n), Some(v)) => Some(format!("{}={}", n, v)),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

/// Closes the browser (and deletes its temporary profile).
pub fn close() {
    let taken = match LIVE.lock() {
        Ok(mut g) => g.take(),
        Err(_) => None,
    };
    if let Some(l) = taken {
        shutdown(l);
    }
}

/// Opens a visible browser window on untitled.stream (for logging in / checking it works).
pub fn open_window(remember_login: bool, log: &dyn Fn(String)) -> Result<(), String> {
    SHOW.store(true, Ordering::SeqCst);
    let mut g = LIVE.lock().map_err(|_| "browser state unavailable".to_string())?;
    if let Some(l) = g.as_ref() {
        if !l.visible {
            if let Some(old) = g.take() {
                shutdown(old);
            }
        }
    }
    if g.is_none() {
        *g = Some(launch(true, remember_login, log)?);
    }
    if let Some(l) = g.as_mut() {
        let sid = l.sid.clone();
        let _ = l.cdp.call(
            Some(sid.as_str()),
            "Page.navigate",
            json!({ "url": "https://untitled.stream/library" }),
            Duration::from_secs(30),
        );
    }
    log("Browser window is open. Log in if you like, then press Download (keep \"Show browser window\" ticked).".to_string());
    Ok(())
}

/// Loads the project page in the background browser and returns HTML that contains the
/// page's data blob (all the app needs; nothing else is passed on).
pub fn fetch_page(page_url: &str, remember_login: bool, log: &dyn Fn(String)) -> Result<String, String> {
    let slug = page_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let show = SHOW.load(Ordering::SeqCst);
    let mut g = LIVE.lock().map_err(|_| "browser state unavailable".to_string())?;
    if let Some(l) = g.as_ref() {
        if l.visible != show {
            if let Some(old) = g.take() {
                shutdown(old);
            }
        }
    }
    if g.is_none() {
        *g = Some(launch(show, remember_login, log)?);
    }
    let r = {
        let live = g.as_mut().ok_or_else(|| "browser is not running".to_string())?;
        navigate_and_wait(live, page_url, &slug, show, log)
    };
    match r {
        Ok(ctx) => Ok(format!("<script>window.__remixContext = {};</script>", ctx)),
        Err(e) => {
            if let Some(l) = g.take() {
                shutdown(l);
            }
            Err(e)
        }
    }
}

// ------------------------------------------------------------------ internals

struct Live {
    browser: Browser,
    cdp: Cdp,
    sid: String,
    ua: String,
    visible: bool,
}

struct Browser {
    child: Child,
    dir: PathBuf,
    delete_dir: bool,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.delete_dir {
            for _ in 0..12 {
                if fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

fn shutdown(mut l: Live) {
    let _ = l.cdp.call(None, "Browser.close", json!({}), Duration::from_secs(3));
    for _ in 0..10 {
        if let Ok(Some(_)) = l.browser.child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    drop(l);
}

fn find_browser() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("UNTITLED_BROWSER") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut c: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles(x86)", "ProgramFiles", "LocalAppData"] {
        if let Ok(base) = std::env::var(var) {
            let b = PathBuf::from(base);
            c.push(b.join("Microsoft").join("Edge").join("Application").join("msedge.exe"));
            c.push(b.join("Google").join("Chrome").join("Application").join("chrome.exe"));
            c.push(b.join("BraveSoftware").join("Brave-Browser").join("Application").join("brave.exe"));
        }
    }
    c.push(PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"));
    c.push(PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in ["microsoft-edge", "google-chrome", "chromium", "chromium-browser"] {
                c.push(dir.join(name));
            }
        }
    }
    c.into_iter().find(|p| p.is_file())
}

fn profile_dir(remember: bool) -> Result<(PathBuf, bool), String> {
    if remember {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
            .unwrap_or_else(std::env::temp_dir);
        let d = base.join("untitled-tools").join("browser-profile");
        fs::create_dir_all(&d).map_err(|e| format!("cannot create browser profile folder: {}", e))?;
        Ok((d, false))
    } else {
        let mut rnd = [0u8; 6];
        getrandom::getrandom(&mut rnd).map_err(|e| format!("no randomness: {}", e))?;
        let tag: String = rnd.iter().map(|b| format!("{:02x}", b)).collect();
        let d = std::env::temp_dir().join(format!("untitled-tools-browser-{}", tag));
        fs::create_dir_all(&d).map_err(|e| format!("cannot create temporary profile: {}", e))?;
        Ok((d, true))
    }
}

struct Cdp {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    id: u64,
}

impl Cdp {
    fn connect(url: &str) -> Result<Cdp, String> {
        let (ws, _) = tungstenite::connect(url).map_err(|e| format!("cannot talk to the browser: {}", e))?;
        let mut c = Cdp { ws, id: 0 };
        if let MaybeTlsStream::Plain(s) = c.ws.get_mut() {
            let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
        }
        Ok(c)
    }

    fn call(&mut self, session: Option<&str>, method: &str, params: Value, wait: Duration) -> Result<Value, String> {
        self.id += 1;
        let id = self.id;
        let mut m = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            m["sessionId"] = json!(s);
        }
        self.ws
            .send(Message::Text(m.to_string()))
            .map_err(|e| format!("browser connection lost: {}", e))?;
        let start = Instant::now();
        loop {
            if start.elapsed() > wait {
                return Err(format!("browser did not answer {}", method));
            }
            match self.ws.read() {
                Ok(Message::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if v["id"].as_u64() == Some(id) {
                            if let Some(e) = v.get("error") {
                                return Err(format!("{}: {}", method, e["message"].as_str().unwrap_or("error")));
                            }
                            return Ok(v["result"].clone());
                        }
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("browser connection lost: {}", e)),
            }
        }
    }
}

fn host_of(href: &str) -> String {
    let r = href.split("://").nth(1).unwrap_or(href);
    r.split('/').next().unwrap_or("").to_ascii_lowercase()
}

fn path_of(href: &str) -> String {
    let r = href.split("://").nth(1).unwrap_or(href);
    match r.find('/') {
        Some(i) => r[i..].split(|c: char| c == '?' || c == '#').next().unwrap_or("").to_string(),
        None => String::new(),
    }
}

fn needs_window(why: &str) -> String {
    format!(
        "NEEDS_WINDOW: {}. Turn on \"Show browser window\" (command line: --show-browser) and run again so you can log in or pass the check there.",
        why
    )
}

const PROBE: &str = "(function(){try{var c=window.__remixContext;if(c){var s=JSON.stringify(c);if(s&&s.indexOf('\"tracks\"')>0)return s;}}catch(e){}return 'NO|'+document.title+'|'+location.href;})()";

fn launch(show: bool, remember: bool, log: &dyn Fn(String)) -> Result<Live, String> {
    let exe = find_browser().ok_or_else(|| {
        "no Chromium-based browser found. Microsoft Edge comes with Windows 10/11; otherwise install Chrome or Edge (or set UNTITLED_BROWSER to its .exe path)".to_string()
    })?;
    let (dir, delete_dir) = profile_dir(remember)?;
    let _ = fs::remove_file(dir.join("DevToolsActivePort"));
    log(match (show, remember) {
        (true, true) => "Starting the browser window (the app's own profile, login is remembered)...".to_string(),
        (true, false) => "Starting the browser window (temporary profile, deleted afterwards)...".to_string(),
        (false, true) => "Starting the background browser (the app's own profile, login is remembered)...".to_string(),
        (false, false) => "Starting the background browser (temporary profile, no window)...".to_string(),
    });

    let mut cmd = Command::new(&exe);
    cmd.arg(format!("--user-data-dir={}", dir.display()))
        .arg("--remote-debugging-port=0")
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-sync",
            "--disable-default-apps",
            "--disable-background-timer-throttling",
            "--disable-renderer-backgrounding",
            "--disable-backgrounding-occluded-windows",
            "--mute-audio",
            "--window-size=1100,800",
        ]);
    if !show {
        cmd.arg("--headless=new");
    }
    cmd.arg("about:blank");
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if delete_dir {
                let _ = fs::remove_dir_all(&dir);
            }
            return Err(format!("could not start the browser: {}", e));
        }
    };
    let mut browser = Browser { child, dir: dir.clone(), delete_dir };

    let port_file = dir.join("DevToolsActivePort");
    let boot = Instant::now();
    let ws_url: String = loop {
        if CANCEL.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        if let Ok(t) = fs::read_to_string(&port_file) {
            let mut l = t.lines();
            if let (Some(port), Some(path)) = (l.next(), l.next()) {
                if !port.trim().is_empty() && path.starts_with('/') {
                    break format!("ws://127.0.0.1:{}{}", port.trim(), path.trim());
                }
            }
        }
        if let Ok(Some(_)) = browser.child.try_wait() {
            return Err("the browser closed right after starting (is another window using the same profile?)".to_string());
        }
        if boot.elapsed() > Duration::from_secs(25) {
            return Err("the browser did not start in time".to_string());
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    let mut cdp = Cdp::connect(&ws_url)?;
    let t10 = Duration::from_secs(10);
    let targets = cdp.call(None, "Target.getTargets", json!({}), t10)?;
    let existing = targets["targetInfos"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["type"].as_str() == Some("page")))
        .and_then(|t| t["targetId"].as_str())
        .map(|s| s.to_string());
    let target_id = match existing {
        Some(t) => t,
        None => {
            let r = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" }), t10)?;
            r["targetId"].as_str().ok_or("browser gave no page")?.to_string()
        }
    };
    let att = cdp.call(None, "Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }), t10)?;
    let sid = att["sessionId"].as_str().ok_or("browser gave no session")?.to_string();

    let ver = cdp.call(None, "Browser.getVersion", json!({}), t10)?;
    let ua = ver["userAgent"]
        .as_str()
        .unwrap_or("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36")
        .replace("HeadlessChrome", "Chrome");
    let _ = cdp.call(Some(sid.as_str()), "Emulation.setUserAgentOverride", json!({ "userAgent": ua }), t10);

    Ok(Live { browser, cdp, sid, ua, visible: show })
}

fn navigate_and_wait(
    live: &mut Live,
    page_url: &str,
    slug: &str,
    show: bool,
    log: &dyn Fn(String),
) -> Result<String, String> {
    let sid = live.sid.clone();
    let s = Some(sid.as_str());
    let t10 = Duration::from_secs(10);
    log(format!("Opening {} ...", page_url));
    let _ = live.cdp.call(s, "Page.navigate", json!({ "url": page_url }), Duration::from_secs(30));

    let limit = if show { Duration::from_secs(600) } else { Duration::from_secs(45) };
    let start = Instant::now();
    let mut off_since: Option<Instant> = None;
    let mut last_nav = Instant::now();
    let (mut said_check, mut said_off, mut said_wait) = (false, false, false);
    if show {
        log("If the site asks you to log in or pass a check, do it in the browser window - the app carries on by itself.".to_string());
    }

    loop {
        if CANCEL.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        if start.elapsed() > limit {
            return Err(if show {
                "timed out waiting in the browser window".to_string()
            } else {
                needs_window("the site did not finish loading invisibly")
            });
        }
        std::thread::sleep(Duration::from_millis(800));
        let val = match live
            .cdp
            .call(s, "Runtime.evaluate", json!({ "expression": PROBE, "returnByValue": true }), t10)
        {
            Ok(v) => v["result"]["value"].as_str().unwrap_or("").to_string(),
            Err(e) => {
                if e.contains("connection lost") {
                    return Err("the browser window was closed".to_string());
                }
                continue;
            }
        };
        if val.starts_with('{') {
            if let Ok(c) = serde_json::from_str::<Value>(&val) {
                if find_project(&c, 0).is_some() {
                    return Ok(val);
                }
            }
            continue;
        }
        let mut parts = val.splitn(3, '|');
        let _ = parts.next();
        let title = parts.next().unwrap_or("").to_string();
        let href = parts.next().unwrap_or("").to_string();
        let challenge = title.contains("Just a moment")
            || title.contains("Attention Required")
            || title.contains("Verify you are human")
            || href.contains("__cf_chl");
        if challenge {
            off_since = None;
            if !said_check {
                said_check = true;
                log("The site is running its browser check - waiting for it...".to_string());
            }
        } else if href.contains(slug) {
            off_since = None;
            if !said_wait {
                said_wait = true;
                log("Project page loaded, waiting for the track list...".to_string());
            }
        } else if !href.is_empty() && href != "about:blank" {
            if off_since.is_none() {
                off_since = Some(Instant::now());
            }
            if !said_off {
                said_off = true;
                log(format!(
                    "The site sent the browser to \"{}\" instead of the project (probably a login).",
                    path_of(&href)
                ));
            }
            let waited = off_since.map(|t| t.elapsed()).unwrap_or_default();
            if !show && waited > Duration::from_secs(10) {
                return Err(needs_window("the site wants a login"));
            }
            if show && waited > Duration::from_secs(8) && last_nav.elapsed() > Duration::from_secs(10) {
                let h = host_of(&href);
                let p = path_of(&href).to_ascii_lowercase();
                let loginish = ["login", "sign", "auth", "oauth", "callback", "register", "join", "account"]
                    .iter()
                    .any(|w| p.contains(w));
                if (h == "untitled.stream" || h == "www.untitled.stream") && !loginish {
                    log("Going back to the project page...".to_string());
                    let _ = live.cdp.call(s, "Page.navigate", json!({ "url": page_url }), Duration::from_secs(30));
                    last_nav = Instant::now();
                    off_since = Some(Instant::now());
                }
            }
        }
    }
}
