//! Core logic for [UNTITLED] TOOLS.
//!
//! Privacy design: the page data contains lots of account info (usernames, emails,
//! profile pictures, session tokens...). We never copy the page data anywhere.
//! We read it into memory, pick out an explicit allow-list of fields (see `Track`),
//! and only those fields can ever reach the disk.

use crate::meta;
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const MAX_PAGE_BYTES: u64 = 30 * 1024 * 1024;
const MAX_AUDIO_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AudioMode {
    Mp3,
    Original,
    Both,
}

impl AudioMode {
    pub fn parse(s: &str) -> AudioMode {
        match s.to_ascii_lowercase().as_str() {
            "original" => AudioMode::Original,
            "both" => AudioMode::Both,
            _ => AudioMode::Mp3,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CookiePolicy {
    /// Ask once, only if the server refuses a download.
    Ask,
    /// Use the background browser's login session for every download.
    Always,
    /// Never use it.
    Never,
}

impl CookiePolicy {
    pub fn parse(s: &str) -> CookiePolicy {
        match s.to_ascii_lowercase().as_str() {
            "always" | "allow" => CookiePolicy::Always,
            "never" => CookiePolicy::Never,
            _ => CookiePolicy::Ask,
        }
    }
}

/// What gets saved. Everything is on by default; the window has a checkbox for each.
#[derive(Clone, Copy)]
pub struct Toggles {
    /// The audio files themselves.
    pub audio: bool,
    /// Write title/artist/album/BPM/key/cover into the MP3 itself.
    pub embed_tags: bool,
    /// Cover images (project cover, and per-song covers if the site has them).
    pub covers: bool,
    /// The public artist name. (Account username, e-mail and profile picture are never saved.)
    pub artist: bool,
    pub track_json: bool,
    pub project_json: bool,
    pub csv: bool,
    pub playlist: bool,
}

impl Default for Toggles {
    fn default() -> Self {
        Toggles {
            audio: true,
            embed_tags: true,
            covers: true,
            artist: true,
            track_json: true,
            project_json: true,
            csv: true,
            playlist: true,
        }
    }
}

#[derive(Clone)]
pub struct Options {
    pub mode: AudioMode,
    pub subfolder: bool,
    pub metadata_only: bool,
    /// Tracks whose owner switched downloads off are skipped unless this is true.
    pub allow_nondownloadable: bool,
    /// Optional manual access token. Memory only: never logged, never written.
    pub token_override: Option<String>,
    /// Load pages with the background browser (falls back to a direct fetch).
    pub use_browser: bool,
    /// Keep the background browser's login in its own profile between runs.
    pub remember_login: bool,
    pub cookies: CookiePolicy,
    /// Called (once) to ask the person for permission; None means "no".
    pub ask: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    pub toggles: Toggles,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: AudioMode::Mp3,
            subfolder: true,
            metadata_only: false,
            allow_nondownloadable: false,
            token_override: None,
            use_browser: true,
            remember_login: false,
            cookies: CookiePolicy::Ask,
            ask: None,
            toggles: Toggles::default(),
        }
    }
}

pub enum Source {
    Url(String),
    File(PathBuf),
}

/// The ONLY data we keep about a track. Anything not listed here is dropped.
#[derive(Clone, Default)]
pub struct Track {
    pub index: usize,
    pub title: String,
    pub slug: String,
    pub duration: Option<f64>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub file_type: Option<String>,
    pub codec: Option<String>,
    pub container: Option<String>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub size_bytes: Option<i64>,
    pub created: Option<f64>,
    pub downloadable: Option<bool>,
    pub ai_generated: Option<bool>,
    pub mp3_url: Option<String>,
    pub original_url: Option<String>,
    pub cover_url: Option<String>,
}

pub struct Project {
    pub title: String,
    pub slug: String,
    pub tracks: Vec<Track>,
    token: Option<String>,
    source_url: Option<String>,
    pub artist: Option<String>,
    pub cover_url: Option<String>,
    pub default_cover: Option<String>,
    pub created: Option<f64>,
}

// ---------------------------------------------------------------- URL handling

/// Accepts only https://untitled.stream/.../project/<slug> and returns the slug.
pub fn parse_project_url(input: &str) -> Result<String, String> {
    let u = input.trim();
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .ok_or_else(|| "link must start with https://".to_string())?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host.to_ascii_lowercase();
    if host != "untitled.stream" && host != "www.untitled.stream" {
        return Err("that is not an untitled.stream link".to_string());
    }
    let path = path.split(|c: char| c == '?' || c == '#').next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let pos = segs
        .iter()
        .position(|s| *s == "project")
        .ok_or_else(|| "link must look like https://untitled.stream/library/project/<id>".to_string())?;
    let slug = segs
        .get(pos + 1)
        .ok_or_else(|| "the link has no project id after /project/".to_string())?;
    if slug.is_empty()
        || slug.len() > 64
        || !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("the project id in the link looks invalid".to_string());
    }
    Ok(slug.to_string())
}

/// Media may only ever be fetched from untitled.stream hosts (stops a doctored page
/// from pointing the app at some other server).
fn allowed_media_url(u: &str) -> bool {
    match u.strip_prefix("https://") {
        Some(r) => {
            let host = r.split('/').next().unwrap_or("");
            !host.contains('@')
                && !host.contains(':')
                && (host == "untitled.stream" || host.ends_with(".untitled.stream"))
        }
        None => false,
    }
}

fn url_ext(u: &str) -> String {
    let p = u.split('?').next().unwrap_or("");
    match p.rsplit_once('.') {
        Some((_, e)) if !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()) => {
            e.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

fn ext_or(u: &str, default: &str) -> String {
    let e = url_ext(u);
    if e.is_empty() {
        default.to_string()
    } else {
        e
    }
}

// ---------------------------------------------------------------- HTTP

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .redirects(3)
        .build()
}

fn describe_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => match code {
            403 | 429 | 503 => format!(
                "server answered HTTP {} (the site's bot check blocked a direct request - turn on \"Use background browser\", or save the page with Ctrl+S and use \"Open saved page...\")",
                code
            ),
            400 | 401 => format!(
                "server answered HTTP {} (it wants a login token - use \"Open saved page...\" with a page saved while logged in, or paste a token under Advanced)",
                code
            ),
            _ => format!("server answered HTTP {}", code),
        },
        other => format!("network error: {}", other),
    }
}

fn fetch_text(url: &str) -> Result<String, String> {
    let resp = agent()
        .get(url)
        .set("Accept", "text/html,application/xhtml+xml")
        .call()
        .map_err(describe_err)?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_PAGE_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn download(
    url: &str,
    dest: &Path,
    token: Option<&str>,
    cookie: Option<&str>,
    ua: Option<&str>,
    cancel: &AtomicBool,
) -> Result<u64, String> {
    if !allowed_media_url(url) {
        return Err("refused: media link is not on untitled.stream".to_string());
    }
    // no redirects: credentials must never be forwarded to another host
    let ag = ureq::AgentBuilder::new()
        .user_agent(ua.unwrap_or(USER_AGENT))
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .redirects(0)
        .build();
    let mut req = ag
        .get(url)
        .set("Referer", "https://untitled.stream/")
        .set("Origin", "https://untitled.stream");
    // links that already carry their own signed token must not get extra credentials
    let signed = url.contains("token=");
    if let Some(t) = token {
        if !signed {
            req = req.set("Authorization", &format!("Bearer {}", t));
        }
    }
    if let Some(c) = cookie {
        if !signed {
            req = req.set("Cookie", c);
        }
    }
    let resp = req.call().map_err(describe_err)?;
    if resp.status() >= 300 {
        return Err(format!("unexpected redirect (HTTP {})", resp.status()));
    }
    let ctype = resp.content_type().to_ascii_lowercase();
    if ctype.starts_with("text/") || ctype.contains("json") {
        return Err(format!("server sent {} instead of audio", ctype));
    }
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    let mut reader = resp.into_reader().take(MAX_AUDIO_BYTES);
    let mut file = fs::File::create(&tmp).map_err(|e| format!("cannot create file: {}", e))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = fs::remove_file(&tmp);
            return Err("cancelled".to_string());
        }
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                drop(file);
                let _ = fs::remove_file(&tmp);
                return Err(format!("download interrupted: {}", e));
            }
        };
        if n == 0 {
            break;
        }
        if let Err(e) = file.write_all(&buf[..n]) {
            drop(file);
            let _ = fs::remove_file(&tmp);
            return Err(format!("cannot write file: {}", e));
        }
        total += n as u64;
    }
    let _ = file.flush();
    drop(file);
    if total == 0 {
        let _ = fs::remove_file(&tmp);
        return Err("server sent an empty file".to_string());
    }
    fs::rename(&tmp, dest).map_err(|e| format!("cannot finish file: {}", e))?;
    Ok(total)
}

/// Downloads a cover image. Returns its bytes and (if `keep`) the saved file name.
fn fetch_image(
    url: &str,
    folder: &Path,
    stem: &str,
    token: Option<&str>,
    ua: Option<&str>,
    cookie: Option<&str>,
    keep: bool,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, Option<String>), String> {
    let tmp = folder.join(format!("{}.img-tmp", stem));
    download(url, &tmp, token, cookie, ua, cancel)?;
    let bytes = fs::read(&tmp).map_err(|e| e.to_string())?;
    let ext = match meta::image_ext(&bytes) {
        Some(e) => e,
        None => {
            let _ = fs::remove_file(&tmp);
            return Err("the cover was not a PNG/JPEG/WebP image".to_string());
        }
    };
    if keep {
        let name = format!("{}.{}", stem, ext);
        let dest = folder.join(&name);
        let _ = fs::remove_file(&dest);
        fs::rename(&tmp, &dest).map_err(|e| format!("cannot save cover: {}", e))?;
        Ok((bytes, Some(name)))
    } else {
        let _ = fs::remove_file(&tmp);
        Ok((bytes, None))
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

// ---------------------------------------------------------------- page parsing

fn sget(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string())
}

fn extract_context(html: &str) -> Option<Value> {
    let mut from = 0usize;
    while let Some(i) = html[from..].find("__remixContext") {
        let abs = from + i;
        from = abs + 14;
        let rest = html[from..].trim_start();
        if let Some(r) = rest.strip_prefix('=') {
            let r = r.trim_start();
            if r.starts_with('{') {
                if let Some(Ok(v)) = serde_json::Deserializer::from_str(r).into_iter::<Value>().next() {
                    return Some(v);
                }
            }
        }
    }
    None
}

pub(crate) fn find_project(v: &Value, depth: usize) -> Option<&Value> {
    if depth > 8 {
        return None;
    }
    match v {
        Value::Object(m) => {
            if m.get("tracks").map_or(false, |t| t.is_array()) && m.get("project").map_or(false, |p| p.is_object()) {
                return Some(v);
            }
            for x in m.values() {
                if let Some(f) = find_project(x, depth + 1) {
                    return Some(f);
                }
            }
            None
        }
        Value::Array(a) => {
            for x in a {
                if let Some(f) = find_project(x, depth + 1) {
                    return Some(f);
                }
            }
            None
        }
        _ => None,
    }
}

fn parse_track(index: usize, t: &Value) -> Track {
    let audio = sget(t, "audio_url");
    let fallback = sget(t, "audio_fallback_url");
    let is_mp3 = |u: &Option<String>| u.as_deref().map_or(false, |x| url_ext(x) == "mp3");
    let mp3_url = if is_mp3(&audio) {
        audio.clone()
    } else if is_mp3(&fallback) {
        fallback.clone()
    } else {
        None
    };
    let key = t.get("key").filter(|k| k.is_object()).map(|k| {
        format!(
            "{}{} {}",
            sget(k, "base_note").unwrap_or_default(),
            sget(k, "accidental").unwrap_or_default(),
            sget(k, "mode").unwrap_or_default()
        )
        .trim()
        .to_string()
    });
    Track {
        index,
        title: sget(t, "title")
            .or_else(|| sget(t, "version_title"))
            .filter(|x| !x.trim().is_empty())
            .unwrap_or_else(|| format!("Track {:02}", index)),
        slug: sget(t, "slug").unwrap_or_default(),
        duration: t.get("duration").and_then(|x| x.as_f64()),
        bpm: t.get("bpm").and_then(|x| x.as_f64()),
        key: key.filter(|k| !k.is_empty()),
        file_type: sget(t, "file_type"),
        codec: sget(t, "codec"),
        container: sget(t, "container"),
        sample_rate: t.get("sample_rate").and_then(|x| x.as_i64()),
        bit_depth: t.get("bit_depth").and_then(|x| x.as_i64()),
        size_bytes: t.get("file_size_bytes").and_then(|x| x.as_i64()),
        created: t.get("time_created").and_then(|x| x.as_f64()),
        downloadable: t.get("downloadable").and_then(|x| x.as_bool()),
        ai_generated: t.get("ai_generated").and_then(|x| x.as_bool()),
        mp3_url,
        original_url: audio.or(fallback),
        cover_url: pick_art(t),
    }
}

/// Picks an uploaded cover image link (only untitled.stream hosts are accepted).
fn pick_art(v: &Value) -> Option<String> {
    for k in [
        "artwork_signed_url",
        "artwork_url",
        "artwork_small_signed_url",
        "artwork_thumbnail_signed_url",
    ] {
        if let Some(s) = sget(v, k) {
            let u = if s.starts_with('/') {
                format!("https://sb.untitled.stream{}", s)
            } else {
                s
            };
            if allowed_media_url(&u) {
                return Some(u);
            }
        }
    }
    None
}

/// Last-resort scan if the structured data is missing: find raw MP3 links.
fn scan_audio_urls(html: &str) -> Vec<String> {
    let needle = "https://sb.untitled.stream/storage/v1/object/";
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0usize;
    while let Some(i) = html[pos..].find(needle) {
        let st = pos + i;
        let rest = &html[st..];
        let end = rest
            .find(|c: char| matches!(c, '"' | '\'' | '\\' | '<' | '>' | ' ' | '\n'))
            .unwrap_or(rest.len());
        let u = &rest[..end];
        pos = st + end.max(needle.len());
        let lower = u.to_ascii_lowercase();
        if (lower.contains("/private-audio/") || lower.contains("/private-transcoded-audio/"))
            && url_ext(u) == "mp3"
            && !out.iter().any(|x| x == u)
        {
            out.push(u.to_string());
        }
    }
    out
}

pub fn load_project(src: &Source, opts: &Options, log: &dyn Fn(String)) -> Result<Project, String> {
    let (html, url_slug) = match src {
        Source::Url(u) => {
            let slug = parse_project_url(u)?;
            let page = format!("https://untitled.stream/library/project/{}", slug);
            let html = if opts.use_browser {
                match crate::browser::fetch_page(&page, opts.remember_login, log) {
                    Ok(h) => h,
                    Err(e) => {
                        if e.starts_with("NEEDS_WINDOW") || e == "cancelled" {
                            return Err(e);
                        }
                        log(format!("Background browser problem: {}", e));
                        log("Trying a direct fetch instead...".to_string());
                        fetch_text(&page)?
                    }
                }
            } else {
                log("Fetching project page...".to_string());
                fetch_text(&page)?
            };
            (html, Some(slug))
        }
        Source::File(p) => {
            log(format!("Reading saved page: {}", p.display()));
            let meta = fs::metadata(p).map_err(|e| format!("cannot open file: {}", e))?;
            if meta.len() > MAX_PAGE_BYTES {
                return Err("that file is too large to be a saved page".to_string());
            }
            let bytes = fs::read(p).map_err(|e| format!("cannot read file: {}", e))?;
            (String::from_utf8_lossy(&bytes).into_owned(), None)
        }
    };

    let ctx = extract_context(&html);
    let mut project = Project {
        title: String::new(),
        slug: url_slug.clone().unwrap_or_default(),
        tracks: Vec::new(),
        token: None,
        artist: None,
        cover_url: None,
        default_cover: None,
        created: None,
        source_url: url_slug
            .as_ref()
            .map(|s| format!("https://untitled.stream/library/project/{}", s)),
    };

    if let Some(c) = &ctx {
        project.token = c
            .pointer("/state/loaderData/root/accessToken")
            .and_then(|x| x.as_str())
            .filter(|x| !x.is_empty())
            .map(|x| x.to_string());
        if let Some(w) = find_project(c, 0) {
            let p = &w["project"];
            if let Some(t) = sget(p, "title") {
                project.title = t;
            }
            if let Some(sl) = sget(p, "slug") {
                project.slug = sl;
            }
            project.artist = sget(p, "artist_name").filter(|x| !x.trim().is_empty());
            project.cover_url = pick_art(p);
            project.default_cover = sget(p, "default_cover_art");
            project.created = p.get("time_created").and_then(|x| x.as_f64());
            let flag = |k: &str| p.get(k).and_then(|x| x.as_bool()) == Some(true);
            if flag("password_protected") || flag("payment_required") {
                return Err("this project is password-protected or paid; this tool does not bypass that".to_string());
            }
            if let Some(arr) = w["tracks"].as_array() {
                for (i, t) in arr.iter().enumerate() {
                    project.tracks.push(parse_track(i + 1, t));
                }
            }
        }
    }

    if project.tracks.is_empty() {
        let urls = scan_audio_urls(&html);
        if !urls.is_empty() {
            log("Structured data not found - using fallback scan (track names will be generic).".to_string());
            for (i, u) in urls.into_iter().enumerate() {
                let mut t = Track::default();
                t.index = i + 1;
                t.title = format!("Track {:02}", i + 1);
                t.mp3_url = Some(u.clone());
                t.original_url = Some(u);
                project.tracks.push(t);
            }
        }
    }

    if project.tracks.is_empty() {
        if ctx.is_none()
            && (html.contains("Just a moment") || html.contains("cf-chl") || html.contains("Enable JavaScript and cookies"))
        {
            return Err("the site showed the app a bot-check page. Open the project in your browser, save the page (Ctrl+S), then use \"Open saved page\"".to_string());
        }
        return Err("no tracks found on that page (is the link right and the project public? If it needs a login, use \"Show browser window\", log in there, and try again)".to_string());
    }
    if project.title.trim().is_empty() {
        project.title = "untitled-project".to_string();
    }
    Ok(project)
}

// ---------------------------------------------------------------- helpers

pub fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    s = s.trim().trim_end_matches(|c: char| c == '.' || c == ' ').to_string();
    if s.is_empty() {
        s = "untitled".to_string();
    }
    let upper = s.to_ascii_uppercase();
    let base = upper.split('.').next().unwrap_or("");
    let reserved = matches!(base, "CON" | "PRN" | "AUX" | "NUL")
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.len() == 4
            && base.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    if s.chars().count() > 120 {
        s = s.chars().take(120).collect();
        s = s.trim_end_matches(|c: char| c == '.' || c == ' ').to_string();
    }
    s
}

fn iso(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn now_iso() -> String {
    iso(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0))
}

fn fmt_dur(s: f64) -> String {
    let t = s.round() as i64;
    format!("{}:{:02}", t / 60, t % 60)
}

fn csv(v: &str) -> String {
    let mut s = v.to_string();
    if s.starts_with(|c: char| matches!(c, '=' | '+' | '-' | '@')) {
        s.insert(0, '\'');
    }
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn plan_downloads(t: &Track, mode: AudioMode) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    let mp3 = t.mp3_url.clone();
    let orig = t.original_url.clone();
    match mode {
        AudioMode::Mp3 => {
            if let Some(u) = mp3.or(orig) {
                let e = ext_or(&u, "mp3");
                v.push((u, e));
            }
        }
        AudioMode::Original => {
            if let Some(u) = orig.or(mp3) {
                let e = ext_or(&u, "bin");
                v.push((u, e));
            }
        }
        AudioMode::Both => {
            if let Some(u) = &mp3 {
                v.push((u.clone(), "mp3".to_string()));
            }
            if let Some(u) = &orig {
                if Some(u) != mp3.as_ref() {
                    v.push((u.clone(), ext_or(u, "bin")));
                }
            }
        }
    }
    v
}

fn track_json(
    t: &Track,
    project: &str,
    artist: Option<&str>,
    files: &[String],
    images: &[String],
    file_meta: &[Value],
    file_dur: Option<f64>,
) -> Value {
    let dur_source = if file_dur.is_some() { "file" } else { "site" };
    json!({
        "index": t.index,
        "title": t.title,
        "artist": artist,
        "track_slug": t.slug,
        "project": project,
        "duration_seconds": file_dur.or(t.duration),
        "duration_source": dur_source,
        "bpm": t.bpm,
        "key": t.key,
        "audio_as_uploaded": {
            "file_type": t.file_type,
            "codec": t.codec,
            "container": t.container,
            "sample_rate_hz": t.sample_rate,
            "bit_depth": t.bit_depth,
            "size_bytes": t.size_bytes
        },
        "files": file_meta,
        "images": images,
        "created_utc": t.created.map(|x| iso(x as i64)),
        "owner_allows_download": t.downloadable,
        "ai_generated": t.ai_generated,
        "saved_files": files
    })
}

// ---------------------------------------------------------------- public jobs

pub fn preview(src: &Source, opts: &Options, log: &dyn Fn(String)) -> Result<(), String> {
    let r = preview_inner(src, opts, log);
    crate::browser::close();
    r
}

fn preview_inner(src: &Source, opts: &Options, log: &dyn Fn(String)) -> Result<(), String> {
    let p = load_project(src, opts, log)?;
    log(format!("Project: {}  ({} tracks)", p.title, p.tracks.len()));
    if opts.toggles.artist {
        if let Some(a) = &p.artist {
            log(format!("Artist: {}", a));
        }
    }
    log(if p.cover_url.is_some() {
        "Cover: an uploaded image exists".to_string()
    } else {
        "Cover: none uploaded (the site shows a generated gradient)".to_string()
    });
    for t in &p.tracks {
        let plan = plan_downloads(t, opts.mode);
        let exts: Vec<&str> = plan.iter().map(|(_, e)| e.as_str()).collect();
        let dur = t.duration.map(fmt_dur).unwrap_or_else(|| "?".to_string());
        let note = if t.downloadable == Some(false) {
            "  (owner disabled downloads)"
        } else {
            ""
        };
        log(format!("  {:02}. {}  [{}]  {}{}", t.index, t.title, exts.join("+"), dur, note));
    }
    log("Kept per track: title, length, BPM, key, audio format info, created date, flags.".to_string());
    log("NOT kept: usernames, artist names, emails, profile pictures, cover art, account IDs, tokens, links.".to_string());
    Ok(())
}

pub fn run(
    src: &Source,
    out_root: &Path,
    opts: &Options,
    log: &dyn Fn(String),
    cancel: &AtomicBool,
) -> Result<(), String> {
    let r = run_inner(src, out_root, opts, log, cancel);
    crate::browser::close();
    r
}

fn run_inner(
    src: &Source,
    out_root: &Path,
    opts: &Options,
    log: &dyn Fn(String),
    cancel: &AtomicBool,
) -> Result<(), String> {
    if !out_root.is_dir() {
        return Err(format!("output folder does not exist: {}", out_root.display()));
    }
    let project = load_project(src, opts, log)?;
    let token = opts
        .token_override
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| project.token.clone());
    let pasted = opts.token_override.as_ref().map_or(false, |t| !t.trim().is_empty());
    if pasted {
        log("Using the access token you pasted (memory only).".to_string());
    } else if token.is_some() {
        log("Found a login token in the page data - using it for this run only (never saved).".to_string());
    } else {
        log("No login token found in the page data.".to_string());
    }
    let folder = if opts.subfolder {
        out_root.join(sanitize(&project.title))
    } else {
        out_root.to_path_buf()
    };
    fs::create_dir_all(&folder).map_err(|e| format!("cannot create folder: {}", e))?;
    log(format!(
        "Project \"{}\" - {} track(s) -> {}",
        project.title,
        project.tracks.len(),
        folder.display()
    ));

    let ua: Option<String> = if opts.use_browser { crate::browser::user_agent() } else { None };
    let mut cookie: Option<String> = if opts.use_browser && opts.cookies == CookiePolicy::Always {
        crate::browser::cookie_header()
    } else {
        None
    };
    let mut asked = false;
    if cookie.is_some() {
        log("Using the background browser's login session for downloads (memory only).".to_string());
    }

    let tg = opts.toggles;
    let save_audio = tg.audio && !opts.metadata_only;
    let artist: Option<&str> = if tg.artist { project.artist.as_deref() } else { None };
    let mut project_cover: Option<Vec<u8>> = None;
    let mut cover_file: Option<String> = None;
    if tg.covers || (tg.embed_tags && save_audio) {
        match &project.cover_url {
            Some(u) => match fetch_image(
                u,
                &folder,
                "cover",
                token.as_deref(),
                ua.as_deref(),
                cookie.as_deref(),
                tg.covers,
                cancel,
            ) {
                Ok((bytes, name)) => {
                    log(format!(
                        "Cover art: {} KB{}",
                        bytes.len() / 1024,
                        name.as_ref().map(|n| format!(" -> {}", n)).unwrap_or_default()
                    ));
                    project_cover = Some(bytes);
                    cover_file = name;
                }
                Err(e) => log(format!("Cover art skipped: {}", e)),
            },
            None => log("No uploaded cover on this project (the site shows a generated gradient), so there is no image to save.".to_string()),
        }
    }

    let width = std::cmp::max(2, project.tracks.len().to_string().len());
    let mut manifest: Vec<Value> = Vec::new();
    let mut playlist: Vec<(Option<f64>, String, String)> = Vec::new();
    let (mut saved, mut skipped, mut failed) = (0usize, 0usize, 0usize);

    for t in &project.tracks {
        if cancel.load(Ordering::Relaxed) {
            log("Cancelled.".to_string());
            break;
        }
        let stem = format!("{:0w$} - {}", t.index, sanitize(&t.title), w = width);
        let mut files: Vec<String> = Vec::new();
        let mut fresh: Vec<String> = Vec::new();
        let blocked = save_audio && t.downloadable == Some(false) && !opts.allow_nondownloadable;
        if blocked {
            log(format!(
                "[{}] skipped (owner disabled downloads): {}  -- tick \"I have permission\" to override",
                t.index, t.title
            ));
            skipped += 1;
        } else if save_audio {
            for (url, ext) in plan_downloads(t, opts.mode) {
                let name = format!("{}.{}", stem, ext);
                let dest = folder.join(&name);
                if dest.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                    log(format!("[{}] already there: {}", t.index, name));
                    files.push(name);
                    continue;
                }
                log(format!("[{}] downloading {}", t.index, name));
                let mut result = download(&url, &dest, token.as_deref(), cookie.as_deref(), ua.as_deref(), cancel);
                let refused = match &result {
                    Err(e) => e.contains("HTTP 400") || e.contains("HTTP 401") || e.contains("HTTP 403"),
                    Ok(_) => false,
                };
                if refused
                    && cookie.is_none()
                    && !asked
                    && opts.cookies != CookiePolicy::Never
                    && crate::browser::is_running()
                {
                    asked = true;
                    let yes = match (&opts.cookies, &opts.ask) {
                        (CookiePolicy::Always, _) => true,
                        (_, Some(f)) => {
                            let g: &(dyn Fn(&str) -> bool + Send + Sync) = &**f;
                            g("The server refused the download. The background browser may be logged in to untitled.stream. Use its login session (cookies) for this run only? They stay in memory and this app never saves them.")
                        }
                        _ => false,
                    };
                    if yes {
                        cookie = crate::browser::cookie_header();
                        if cookie.is_some() {
                            log("      retrying with the background browser's login session (this run only)...".to_string());
                            result = download(&url, &dest, token.as_deref(), cookie.as_deref(), ua.as_deref(), cancel);
                        } else {
                            log("      the background browser has no login session to share.".to_string());
                        }
                    }
                }
                match result {
                    Ok(n) => {
                        saved += 1;
                        log(format!("      saved {} KB", n / 1024));
                        fresh.push(name.clone());
                        files.push(name);
                    }
                    Err(e) => {
                        failed += 1;
                        log(format!("      FAILED: {}", e));
                    }
                }
            }
        }
        // song cover (only when the site gave this song its own artwork)
        let mut images: Vec<String> = Vec::new();
        let mut tcover: Option<Vec<u8>> = None;
        if let Some(u) = &t.cover_url {
            if tg.covers || (tg.embed_tags && !files.is_empty()) {
                match fetch_image(
                    u,
                    &folder,
                    &stem,
                    token.as_deref(),
                    ua.as_deref(),
                    cookie.as_deref(),
                    tg.covers,
                    cancel,
                ) {
                    Ok((bytes, name)) => {
                        tcover = Some(bytes);
                        if let Some(n) = name {
                            images.push(n);
                        }
                    }
                    Err(e) => log(format!("      song cover skipped: {}", e)),
                }
            }
        }

        // tags inside the MP3, then facts read back from the files themselves
        let mut file_meta: Vec<Value> = Vec::new();
        let mut file_dur: Option<f64> = None;
        for f in &files {
            let p = folder.join(f);
            if tg.embed_tags && fresh.contains(f) && f.to_ascii_lowercase().ends_with(".mp3") {
                let cover_bytes: Option<&[u8]> = tcover.as_deref().or(project_cover.as_deref());
                let tw = meta::TagWrite {
                    title: &t.title,
                    artist,
                    album: &project.title,
                    track_no: t.index as u32,
                    bpm: t.bpm,
                    key: t.key.as_deref(),
                    cover: cover_bytes,
                };
                match meta::write_mp3_tags(&p, &tw) {
                    Ok(()) => log(format!("      tags written into {}", f)),
                    Err(e) => log(format!("      could not write tags: {}", e)),
                }
            }
            let fi = meta::read_info(&p);
            if file_dur.is_none() && fi.duration_secs > 0.0 {
                file_dur = Some(round2(fi.duration_secs));
            }
            file_meta.push(json!({
                "file": f,
                "format": fi.ext,
                "size_bytes": fi.size_bytes,
                "duration_seconds": round2(fi.duration_secs),
                "bitrate_kbps": fi.bitrate_kbps,
                "sample_rate_hz": fi.sample_rate,
                "channels": fi.channels,
                "bit_depth": fi.bit_depth
            }));
        }

        let tj = track_json(t, &project.title, artist, &files, &images, &file_meta, file_dur);
        if tg.track_json {
            if let Ok(text) = serde_json::to_string_pretty(&tj) {
                let _ = fs::write(folder.join(format!("{}.json", stem)), text);
            }
        }
        if let Some(f) = files
            .iter()
            .find(|f| f.to_ascii_lowercase().ends_with(".mp3"))
            .or_else(|| files.first())
        {
            playlist.push((file_dur.or(t.duration), t.title.clone(), f.clone()));
        }
        manifest.push(tj);
    }

    // project-level files
    let pj = json!({
        "tool": "[UNTITLED] TOOLS",
        "exported_utc": now_iso(),
        "source_page": project.source_url,
        "project_title": project.title,
        "artist": artist,
        "project_slug": project.slug,
        "default_cover_art": project.default_cover,
        "cover_image": cover_file,
        "project_created_utc": project.created.map(|x| iso(x as i64)),
        "track_count": project.tracks.len(),
        "tracks": manifest
    });
    if tg.project_json {
        if let Ok(text) = serde_json::to_string_pretty(&pj) {
            let _ = fs::write(folder.join("_project.json"), text);
        }
    }
    if tg.csv {
        let mut c = String::from("index,title,artist,duration_seconds,bpm,key,file_type,saved_files\n");
        for t in &project.tracks {
            let files = manifest_files(&pj, t.index);
            let dur = pj["tracks"]
                .as_array()
                .and_then(|a| a.iter().find(|x| x["index"].as_u64() == Some(t.index as u64)))
                .and_then(|x| x["duration_seconds"].as_f64());
            c.push_str(&format!(
                "{},{},{},{},{},{},{},{}\n",
                t.index,
                csv(&t.title),
                csv(artist.unwrap_or("")),
                dur.map(|x| format!("{:.2}", x)).unwrap_or_default(),
                t.bpm.map(|x| x.to_string()).unwrap_or_default(),
                csv(t.key.as_deref().unwrap_or("")),
                csv(t.file_type.as_deref().unwrap_or("")),
                csv(&files.join(" | "))
            ));
        }
        let _ = fs::write(folder.join("_tracks.csv"), c);
    }
    if tg.playlist && !playlist.is_empty() {
        let mut m = String::from("#EXTM3U\n");
        for (d, title, file) in &playlist {
            m.push_str(&format!(
                "#EXTINF:{},{}\n{}\n",
                d.map(|x| x.round() as i64).unwrap_or(-1),
                title.replace('\n', " "),
                file
            ));
        }
        let _ = fs::write(folder.join("_playlist.m3u8"), m);
    }

    log(format!(
        "Finished \"{}\": {} saved, {} skipped, {} failed.",
        project.title, saved, skipped, failed
    ));
    if failed > 0 {
        log("Tip: if every download was refused (HTTP 400/401/403) the site wants a login. Click \"Show browser window\", log in to untitled.stream there, then run again.".to_string());
    }
    Ok(())
}

fn manifest_files(pj: &Value, index: usize) -> Vec<String> {
    pj["tracks"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["index"].as_u64() == Some(index as u64)))
        .and_then(|t| t["saved_files"].as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- extra tools

/// Writes playlist.m3u8 for every .mp3 in a folder (local only, no network).
pub fn build_playlist(dir: &Path) -> Result<usize, String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map_err(|e| format!("cannot read folder: {}", e))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.to_ascii_lowercase().ends_with(".mp3"))
        .collect();
    names.sort();
    if names.is_empty() {
        return Err("no .mp3 files in that folder".to_string());
    }
    let mut out = String::from("#EXTM3U\n");
    for n in &names {
        out.push_str(n);
        out.push('\n');
    }
    fs::write(dir.join("playlist.m3u8"), out).map_err(|e| format!("cannot write playlist: {}", e))?;
    Ok(names.len())
}

/// Checks that every file listed in _project.json exists and is not empty.
pub fn verify_folder(dir: &Path, log: &dyn Fn(String)) -> Result<(), String> {
    let txt = fs::read_to_string(dir.join("_project.json"))
        .map_err(|_| "no _project.json in that folder (pick a folder made by this tool)".to_string())?;
    let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
    let empty: Vec<Value> = Vec::new();
    let tracks = v["tracks"].as_array().unwrap_or(&empty);
    let (mut ok, mut bad) = (0usize, 0usize);
    for t in tracks {
        let files = t["saved_files"].as_array().unwrap_or(&empty);
        if files.is_empty() {
            bad += 1;
            log(format!("  no audio saved for: {}", t["title"].as_str().unwrap_or("?")));
        }
        for f in files.iter().filter_map(|x| x.as_str()) {
            if f.contains('/') || f.contains('\\') {
                continue;
            }
            match fs::metadata(dir.join(f)) {
                Ok(m) if m.len() > 0 => ok += 1,
                _ => {
                    bad += 1;
                    log(format!("  MISSING or empty: {}", f));
                }
            }
        }
    }
    log(format!("Verify: {} file(s) fine, {} problem(s).", ok, bad));
    Ok(())
}
