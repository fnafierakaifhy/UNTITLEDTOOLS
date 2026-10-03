//! Metadata that lives INSIDE the audio files: tags, length, bitrate, cover art.
//! The Library reads everything from here, so it works even if no .json was saved.

use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::{ItemKey, Tag, TagType};
use std::path::Path;

#[derive(Clone, Default)]
pub struct FileInfo {
    pub ext: String,
    pub size_bytes: u64,
    pub duration_secs: f64,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub bit_depth: Option<u8>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_no: Option<u32>,
    pub bpm: Option<String>,
    pub key: Option<String>,
    pub has_cover: bool,
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.trim().to_string())
    }
}

/// Best effort: never fails, unreadable files just give an empty record.
pub fn read_info(path: &Path) -> FileInfo {
    let mut info = FileInfo::default();
    info.ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    info.size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if let Ok(tf) = lofty::read_from_path(path) {
        let p = tf.properties();
        info.duration_secs = p.duration().as_secs_f64();
        info.bitrate_kbps = p.audio_bitrate().or_else(|| p.overall_bitrate());
        info.sample_rate = p.sample_rate();
        info.channels = p.channels();
        info.bit_depth = p.bit_depth();
        if let Some(tag) = tf.primary_tag().or_else(|| tf.first_tag()) {
            info.title = tag.title().and_then(|c| non_empty(c.to_string()));
            info.artist = tag.artist().and_then(|c| non_empty(c.to_string()));
            info.album = tag.album().and_then(|c| non_empty(c.to_string()));
            info.track_no = tag.track();
            info.bpm = tag.get_string(&ItemKey::Bpm).and_then(|s| non_empty(s.to_string()));
            info.key = tag
                .get_string(&ItemKey::InitialKey)
                .and_then(|s| non_empty(s.to_string()));
            info.has_cover = !tag.pictures().is_empty();
        }
    }
    info
}

/// The cover picture embedded in the file, if any.
pub fn read_cover(path: &Path) -> Option<Vec<u8>> {
    let tf = lofty::read_from_path(path).ok()?;
    let tag = tf.primary_tag().or_else(|| tf.first_tag())?;
    let pics = tag.pictures();
    let pic = pics
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pics.first())?;
    Some(pic.data().to_vec())
}

pub struct TagWrite<'a> {
    pub title: &'a str,
    pub artist: Option<&'a str>,
    pub album: &'a str,
    pub track_no: u32,
    pub bpm: Option<f64>,
    pub key: Option<&'a str>,
    pub cover: Option<&'a [u8]>,
}

pub fn image_ext(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

fn sniff_mime(b: &[u8]) -> Option<MimeType> {
    match image_ext(b) {
        Some("png") => Some(MimeType::Png),
        Some("jpg") => Some(MimeType::Jpeg),
        _ => None,
    }
}

/// Writes an ID3v2 tag into an MP3 (replacing any tag it had).
pub fn write_mp3_tags(path: &Path, t: &TagWrite) -> Result<(), String> {
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(t.title.to_string());
    if let Some(a) = t.artist {
        tag.set_artist(a.to_string());
    }
    tag.set_album(t.album.to_string());
    tag.set_track(t.track_no);
    if let Some(b) = t.bpm {
        let _ = tag.insert_text(ItemKey::Bpm, format!("{}", b.round() as i64));
    }
    if let Some(k) = t.key {
        let _ = tag.insert_text(ItemKey::InitialKey, k.to_string());
    }
    if let Some(c) = t.cover {
        if let Some(m) = sniff_mime(c) {
            tag.push_picture(Picture::new_unchecked(
                PictureType::CoverFront,
                Some(m),
                None,
                c.to_vec(),
            ));
        }
    }
    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("{}", e))
}

/// Decodes PNG/JPEG/WebP bytes into a small RGBA picture (max side `max`).
pub fn decode_image(bytes: &[u8], max: u32) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = img.thumbnail(max, max);
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Some((w, h, rgba.into_raw()))
}

// ---------------------------------------------------------------- default covers

fn hsl(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h % 360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    ]
}

/// Colours of untitled's generated cover. "gradient-10" was measured from a real
/// project page; other ids get a stable gradient of their own (not an exact copy).
pub fn gradient_colors(id: &str) -> ([u8; 3], [u8; 3]) {
    if id == "gradient-10" {
        return ([137, 217, 229], [96, 26, 210]);
    }
    let mut h: u32 = 2166136261;
    for b in id.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32;
    (hsl(hue, 0.65, 0.72), hsl(hue + 55.0, 0.75, 0.42))
}

pub fn gradient_image(id: &str, n: u32) -> (u32, u32, Vec<u8>) {
    let (a, b) = gradient_colors(id);
    let mut px: Vec<u8> = Vec::with_capacity((n * n * 4) as usize);
    let denom = (2 * (n - 1)).max(1) as f32;
    for y in 0..n {
        for x in 0..n {
            let t = (x + y) as f32 / denom;
            for k in 0..3 {
                let v = a[k] as f32 + (b[k] as f32 - a[k] as f32) * t;
                px.push(v.round() as u8);
            }
            px.push(255);
        }
    }
    (n, n, px)
}
