//! Small audio player (rodio). Lives on the window thread.

use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct PlayItem {
    pub path: PathBuf,
    pub title: String,
    pub album: String,
    pub duration: f64,
    pub cover_key: String,
}

pub struct Player {
    stream: Option<(OutputStream, OutputStreamHandle)>,
    sink: Option<Sink>,
    pub queue: Vec<PlayItem>,
    pub index: usize,
    started: Option<Instant>,
    base: f64,
    paused: bool,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: bool,
    pub error: Option<String>,
}

impl Player {
    pub fn new() -> Player {
        Player {
            stream: None,
            sink: None,
            queue: Vec::new(),
            index: 0,
            started: None,
            base: 0.0,
            paused: true,
            volume: 0.8,
            shuffle: false,
            repeat: false,
            error: None,
        }
    }

    pub fn active(&self) -> bool {
        !self.queue.is_empty()
    }

    pub fn is_playing(&self) -> bool {
        self.sink.is_some() && !self.paused
    }

    pub fn current(&self) -> Option<&PlayItem> {
        self.queue.get(self.index)
    }

    pub fn current_path(&self) -> Option<PathBuf> {
        self.current().map(|i| i.path.clone())
    }

    pub fn play_queue(&mut self, items: Vec<PlayItem>, start: usize) {
        self.queue = items;
        self.index = start.min(self.queue.len().saturating_sub(1));
        self.start_current(0.0);
    }

    fn start_current(&mut self, offset: f64) {
        self.error = None;
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        let item = match self.queue.get(self.index) {
            Some(i) => i.clone(),
            None => return,
        };
        if self.stream.is_none() {
            match OutputStream::try_default() {
                Ok(s) => self.stream = Some(s),
                Err(e) => {
                    self.error = Some(format!("no audio device: {}", e));
                    return;
                }
            }
        }
        let handle = match &self.stream {
            Some((_, h)) => h.clone(),
            None => return,
        };
        let sink = match Sink::try_new(&handle) {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(format!("cannot start playback: {}", e));
                return;
            }
        };
        let file = match File::open(&item.path) {
            Ok(f) => f,
            Err(e) => {
                self.error = Some(format!("cannot open file: {}", e));
                return;
            }
        };
        let dec = match Decoder::new(BufReader::new(file)) {
            Ok(d) => d,
            Err(e) => {
                self.error = Some(format!("cannot play this file: {}", e));
                return;
            }
        };
        sink.set_volume(self.volume);
        if offset > 0.5 {
            sink.append(dec.skip_duration(Duration::from_secs_f64(offset)));
        } else {
            sink.append(dec);
        }
        self.sink = Some(sink);
        self.base = offset.max(0.0);
        self.started = Some(Instant::now());
        self.paused = false;
    }

    pub fn position(&self) -> f64 {
        match self.started {
            Some(t) if !self.paused => self.base + t.elapsed().as_secs_f64(),
            _ => self.base,
        }
    }

    pub fn toggle_pause(&mut self) {
        if self.sink.is_none() {
            self.start_current(0.0);
            return;
        }
        let pos = self.position();
        if self.paused {
            if let Some(s) = &self.sink {
                s.play();
            }
            self.started = Some(Instant::now());
            self.paused = false;
        } else {
            if let Some(s) = &self.sink {
                s.pause();
            }
            self.base = pos;
            self.started = None;
            self.paused = true;
        }
    }

    pub fn seek(&mut self, secs: f64) {
        let was_paused = self.paused && self.sink.is_some();
        self.start_current(secs.max(0.0));
        if was_paused {
            self.toggle_pause();
        }
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v;
        if let Some(s) = &self.sink {
            s.set_volume(v);
        }
    }

    pub fn next(&mut self) {
        self.advance();
    }

    pub fn prev(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        if self.position() > 3.0 || self.index == 0 {
            self.start_current(0.0);
        } else {
            self.index -= 1;
            self.start_current(0.0);
        }
    }

    fn stop_playback(&mut self) {
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        self.started = None;
        self.base = 0.0;
        self.paused = true;
    }

    fn advance(&mut self) {
        let n = self.queue.len();
        if n == 0 {
            return;
        }
        let next = if self.shuffle && n > 1 {
            let r = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as usize)
                .unwrap_or(0)
                % n;
            if r == self.index {
                (r + 1) % n
            } else {
                r
            }
        } else {
            self.index + 1
        };
        if next >= n {
            if self.repeat {
                self.index = 0;
                self.start_current(0.0);
            } else {
                self.stop_playback();
            }
        } else {
            self.index = next;
            self.start_current(0.0);
        }
    }

    /// Call every frame: moves on when a song ends.
    pub fn tick(&mut self) {
        let done = match &self.sink {
            Some(s) => !self.paused && s.empty(),
            None => false,
        };
        if done {
            self.advance();
        }
    }
}
