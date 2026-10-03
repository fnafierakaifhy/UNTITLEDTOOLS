//! Scans a folder of downloaded projects. Everything shown comes from the audio files
//! themselves (tags, length, bitrate, embedded cover). A _project.json is only used
//! for extras if it happens to be there.

use crate::meta::{self, FileInfo};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const AUDIO_EXTS: [&str; 5] = ["mp3", "wav", "flac", "ogg", "m4a"];

#[derive(Clone)]
pub struct LibTrack {
    pub path: PathBuf,
    pub alt_paths: Vec<PathBuf>,
    pub title: String,
    pub artist: Option<String>,
    pub track_no: Option<u32>,
    pub info: FileInfo,
    pub modified: Option<SystemTime>,
}

#[derive(Clone)]
pub struct LibProject {
    pub dir: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub tracks: Vec<LibTrack>,
    pub total_secs: f64,
    pub total_bytes: u64,
    pub cover: Option<(u32, u32, Vec<u8>)>,
    pub cover_source: String,
    pub gradient_id: String,
    pub newest: Option<SystemTime>,
    pub has_json: bool,
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn is_audio(p: &Path) -> bool {
    let e = ext_of(p);
    AUDIO_EXTS.iter().any(|x| *x == e)
}

fn has_audio(dir: &Path) -> bool {
    match fs::read_dir(dir) {
        Ok(rd) => rd.flatten().any(|e| is_audio(&e.path())),
        Err(_) => false,
    }
}

fn clean_stem(stem: &str) -> String {
    let digits = stem.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 4 {
        if let Some(r) = stem[digits..].strip_prefix(" - ") {
            if !r.trim().is_empty() {
                return r.trim().to_string();
            }
        }
    }
    stem.to_string()
}

fn number_prefix(stem: &str) -> Option<u32> {
    let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 4 {
        None
    } else {
        digits.parse().ok()
    }
}

pub fn scan(root: &Path) -> Vec<LibProject> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if has_audio(root) {
        dirs.push(root.to_path_buf());
    }
    if let Ok(rd) = fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && has_audio(&p) {
                dirs.push(p);
            }
        }
    }
    let mut out: Vec<LibProject> = Vec::new();
    for d in dirs {
        if let Some(p) = load_dir(&d) {
            out.push(p);
        }
    }
    out.sort_by(|a, b| b.newest.cmp(&a.newest));
    out
}

fn load_dir(dir: &Path) -> Option<LibProject> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_audio(p))
        .collect();
    files.sort_by_key(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });
    if files.is_empty() {
        return None;
    }

    // one entry per song: the MP3 if there is one, other formats are listed as extras
    let mut groups: Vec<(String, Vec<PathBuf>)> = Vec::new();
    for f in files {
        let stem = f
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        match groups.iter_mut().find(|g| g.0 == stem) {
            Some(g) => g.1.push(f),
            None => groups.push((stem, vec![f])),
        }
    }

    let pj: Option<Value> = fs::read_to_string(dir.join("_project.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());

    let mut tracks: Vec<LibTrack> = Vec::new();
    for (stem, paths) in groups {
        let pos = paths.iter().position(|p| ext_of(p) == "mp3").unwrap_or(0);
        let main = paths[pos].clone();
        let alt: Vec<PathBuf> = paths
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != pos)
            .map(|(_, p)| p.clone())
            .collect();
        let info = meta::read_info(&main);
        let title = info.title.clone().unwrap_or_else(|| clean_stem(&stem));
        let track_no = info.track_no.or_else(|| number_prefix(&stem));
        let modified = fs::metadata(&main).and_then(|m| m.modified()).ok();
        tracks.push(LibTrack {
            path: main,
            alt_paths: alt,
            title,
            artist: info.artist.clone(),
            track_no,
            info,
            modified,
        });
    }
    tracks.sort_by_key(|t| t.track_no.unwrap_or(u32::MAX));

    let dir_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Untitled".to_string());
    let title = tracks
        .iter()
        .find_map(|t| t.info.album.clone())
        .or_else(|| pj.as_ref().and_then(|v| v["project_title"].as_str().map(|s| s.to_string())))
        .unwrap_or(dir_name);
    let artist = tracks
        .iter()
        .find_map(|t| t.artist.clone())
        .or_else(|| pj.as_ref().and_then(|v| v["artist"].as_str().map(|s| s.to_string())));
    let gradient_id = pj
        .as_ref()
        .and_then(|v| v["default_cover_art"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| title.clone());

    // cover: embedded in a file first, then a cover.* image next to the files
    let mut cover: Option<(u32, u32, Vec<u8>)> = None;
    let mut cover_source = String::from("generated gradient (untitled's default cover)");
    for t in &tracks {
        if t.info.has_cover {
            if let Some(b) = meta::read_cover(&t.path) {
                if let Some(img) = meta::decode_image(&b, 512) {
                    cover = Some(img);
                    cover_source = format!("embedded in {}", t.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                    break;
                }
            }
        }
    }
    if cover.is_none() {
        for name in ["cover.png", "cover.jpg", "cover.jpeg", "cover.webp"] {
            let p = dir.join(name);
            if let Ok(b) = fs::read(&p) {
                if let Some(img) = meta::decode_image(&b, 512) {
                    cover = Some(img);
                    cover_source = format!("image file {}", name);
                    break;
                }
            }
        }
    }

    let total_secs = tracks.iter().map(|t| t.info.duration_secs).sum();
    let total_bytes = tracks.iter().map(|t| t.info.size_bytes).sum();
    let newest = tracks.iter().filter_map(|t| t.modified).max();
    Some(LibProject {
        dir: dir.to_path_buf(),
        title,
        artist,
        tracks,
        total_secs,
        total_bytes,
        cover,
        cover_source,
        gradient_id,
        newest,
        has_json: pj.is_some(),
    })
}

// ---------------------------------------------------------------- remembered folder

fn settings_file() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("untitled-tools").join("settings.txt"))
}

pub fn load_root() -> Option<PathBuf> {
    let t = fs::read_to_string(settings_file()?).ok()?;
    for l in t.lines() {
        if let Some(v) = l.strip_prefix("library=") {
            let p = PathBuf::from(v.trim());
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    None
}

pub fn save_root(p: &Path) {
    if let Some(f) = settings_file() {
        if let Some(d) = f.parent() {
            let _ = fs::create_dir_all(d);
        }
        let _ = fs::write(f, format!("library={}\n", p.display()));
    }
}
