//! Tiny local web server. Binds to 127.0.0.1 ONLY.
//! Guards: Host + Origin checks (DNS-rebinding / cross-site) and a random per-launch
//! token that only the served page knows.

use crate::grab::{self, AudioMode, Options, Source};
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

const PAGE: &str = include_str!("ui.html");

struct State {
    token: String,
    port: u16,
    log: Mutex<Vec<String>>,
    busy: AtomicBool,
    cancel: AtomicBool,
    outer: Arc<dyn Fn(String) + Send + Sync>,
}

impl State {
    fn push(&self, m: String) {
        if let Ok(mut g) = self.log.lock() {
            g.push(m.clone());
        }
        (self.outer)(m);
    }
}

fn random_token() -> String {
    let mut b = [0u8; 24];
    getrandom::getrandom(&mut b).expect("operating system randomness is unavailable");
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn header(req: &Request, name: &'static str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

fn respond(req: Request, code: u16, ctype: &str, body: &str) {
    let mut r = Response::from_string(body.to_string()).with_status_code(code);
    let extra: [(&str, &str); 5] = [
        ("Content-Type", ctype),
        ("Cache-Control", "no-store"),
        ("X-Frame-Options", "DENY"),
        ("X-Content-Type-Options", "nosniff"),
        (
            "Content-Security-Policy",
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    ];
    for (k, v) in extra.iter() {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            r = r.with_header(h);
        }
    }
    let _ = req.respond(r);
}

fn jerr(req: Request, code: u16, msg: &str) {
    respond(req, code, "application/json", &json!({ "error": msg }).to_string());
}

fn host_ok(h: &str, port: u16) -> bool {
    let h = h.to_ascii_lowercase();
    h == format!("127.0.0.1:{}", port) || h == format!("localhost:{}", port) || h == format!("[::1]:{}", port)
}

fn origin_ok(o: &str, port: u16) -> bool {
    o == format!("http://127.0.0.1:{}", port) || o == format!("http://localhost:{}", port)
}

fn authed(req: &Request, st: &State) -> bool {
    header(req, "X-Token").as_deref() == Some(st.token.as_str())
}

fn handle(mut req: Request, st: &Arc<State>) {
    let method = req.method().clone();
    let full = req.url().to_string();
    let (path, query) = match full.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (full.clone(), String::new()),
    };
    let host = header(&req, "Host").unwrap_or_default();
    if !host_ok(&host, st.port) {
        return respond(req, 403, "text/plain", "bad host");
    }
    if let Some(o) = header(&req, "Origin") {
        if !origin_ok(&o, st.port) {
            return respond(req, 403, "text/plain", "bad origin");
        }
    }

    match (method, path.as_str()) {
        (Method::Get, "/") => {
            let page = PAGE.replace("__TOKEN__", &st.token);
            respond(req, 200, "text/html; charset=utf-8", &page);
        }
        (Method::Get, "/api/status") => {
            if !authed(&req, st) {
                return jerr(req, 401, "unauthorized");
            }
            let since = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("since="))
                .and_then(|x| x.parse::<usize>().ok())
                .unwrap_or(0);
            let body = match st.log.lock() {
                Ok(g) => {
                    let lines: Vec<&String> = g.iter().skip(since).collect();
                    json!({ "busy": st.busy.load(Ordering::SeqCst), "next": g.len(), "lines": lines })
                }
                Err(_) => json!({ "error": "log unavailable" }),
            };
            respond(req, 200, "application/json", &body.to_string());
        }
        (Method::Post, "/api/cancel") => {
            if !authed(&req, st) {
                return jerr(req, 401, "unauthorized");
            }
            st.cancel.store(true, Ordering::SeqCst);
            crate::browser::cancel(true);
            respond(req, 200, "application/json", "{\"ok\":true}");
        }
        (Method::Post, "/api/run") => {
            if !authed(&req, st) {
                return jerr(req, 401, "unauthorized");
            }
            let mut body = String::new();
            if req.as_reader().take(65536).read_to_string(&mut body).is_err() {
                return jerr(req, 400, "unreadable request");
            }
            let v: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(_) => return jerr(req, 400, "invalid JSON"),
            };
            let urls: Vec<String> = v["urls"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if urls.is_empty() || urls.len() > 50 {
                return jerr(req, 400, "give between 1 and 50 links");
            }
            let preview = v["preview"].as_bool().unwrap_or(false);
            let dir = PathBuf::from(v["folder"].as_str().unwrap_or("").trim());
            if !preview && (!dir.is_absolute() || !dir.is_dir()) {
                return jerr(req, 400, "folder must be an existing absolute path");
            }
            let opts = Options {
                mode: AudioMode::parse(v["mode"].as_str().unwrap_or("mp3")),
                subfolder: v["subfolder"].as_bool().unwrap_or(true),
                metadata_only: v["metadata_only"].as_bool().unwrap_or(false),
                allow_nondownloadable: v["allow_nd"].as_bool().unwrap_or(false),
                use_browser: v["use_browser"].as_bool().unwrap_or(true),
                remember_login: false,
                cookies: if v["use_cookies"].as_bool().unwrap_or(false) {
                    grab::CookiePolicy::Always
                } else {
                    grab::CookiePolicy::Never
                },
                ask: None,
                toggles: grab::Toggles::default(),
                token_override: v["token"]
                    .as_str()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            };
            if st.busy.swap(true, Ordering::SeqCst) {
                return jerr(req, 409, "a job is already running");
            }
            st.cancel.store(false, Ordering::SeqCst);
            crate::browser::cancel(false);
            crate::browser::set_show_window(v["show_window"].as_bool().unwrap_or(false));
            let st2 = st.clone();
            std::thread::spawn(move || {
                let logf = |m: String| st2.push(m);
                for u in urls {
                    if st2.cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    let src = Source::Url(u);
                    let r = if preview {
                        grab::preview(&src, &opts, &logf)
                    } else {
                        grab::run(&src, &dir, &opts, &logf, &st2.cancel)
                    };
                    if let Err(e) = r {
                        logf(format!("ERROR: {}", e));
                    }
                }
                logf("Done.".to_string());
                st2.busy.store(false, Ordering::SeqCst);
            });
            respond(req, 200, "application/json", "{\"ok\":true}");
        }
        _ => respond(req, 404, "text/plain", "not found"),
    }
}

/// Blocks until `stop` becomes true (or the socket fails).
pub fn serve(port: u16, stop: Arc<AtomicBool>, outer: Arc<dyn Fn(String) + Send + Sync>) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    let st = Arc::new(State {
        token: random_token(),
        port,
        log: Mutex::new(Vec::new()),
        busy: AtomicBool::new(false),
        cancel: AtomicBool::new(false),
        outer: outer.clone(),
    });
    (outer)(format!("Server running at http://127.0.0.1:{}/  (this computer only)", port));
    while !stop.load(Ordering::SeqCst) {
        match server.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(req)) => handle(req, &st),
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    (outer)("Server stopped.".to_string());
    Ok(())
}
