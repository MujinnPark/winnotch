//! Now Playing info + controls + lyrics.
//! Primary source: Windows SMTC (Spotify, browsers, Media Player, ...).
//! Fallback: VLC 3.x (no SMTC support) via its window title + global media keys.
//! Lyrics: looked up on LRCLIB (https://lrclib.net) by title/artist, only when the
//! lyrics panel is open, using the `curl` that ships with Windows 10/11.

use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub use crate::lyrics::Lyrics;
use crate::lyrics::{clean_title, parse_lrc, plain_lines, url_encode};

/// Decoded album art, RGBA8, already downscaled.
pub struct ArtImage {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

#[derive(Default, Clone)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub art: Option<Arc<ArtImage>>,
    /// Bumps whenever `art` changes, so the UI knows when to re-upload the texture.
    pub art_id: u64,
    /// Playback position (ms) at the moment `stamp` was taken. `dur_ms` 0 = unknown.
    pub pos_ms: u64,
    pub dur_ms: u64,
    pub stamp: Option<Instant>,
}

impl NowPlaying {
    /// Current position, advanced by the time since the last snapshot while playing.
    pub fn live_pos_ms(&self) -> u64 {
        let mut p = self.pos_ms;
        if self.playing {
            if let Some(s) = self.stamp {
                p += s.elapsed().as_millis() as u64;
            }
        }
        if self.dur_ms > 0 {
            p.min(self.dur_ms)
        } else {
            p
        }
    }
}

#[derive(Clone, Copy)]
pub enum MediaCmd {
    PlayPause,
    Next,
    Prev,
    /// Jump to this position (ms) in the current track.
    Seek(u64),
}

#[derive(Clone)]
pub enum LyricsStatus {
    Idle,
    Loading,
    Found(Arc<Lyrics>),
    NotFound,
    /// Couldn't reach the lyrics service (offline, curl missing, ...).
    Failed,
}

struct LyricsSlot {
    key: String,
    status: LyricsStatus,
}

pub struct MediaHandle {
    pub state: Arc<Mutex<NowPlaying>>,
    tx: mpsc::Sender<MediaCmd>,
    lyrics: Arc<Mutex<LyricsSlot>>,
}

impl MediaHandle {
    pub fn spawn() -> Self {
        let state = Arc::new(Mutex::new(NowPlaying::default()));
        let (tx, rx) = mpsc::channel();
        let st = state.clone();
        thread::spawn(move || worker(st, rx));
        let lyrics = Arc::new(Mutex::new(LyricsSlot {
            key: String::new(),
            status: LyricsStatus::Idle,
        }));
        Self { state, tx, lyrics }
    }

    pub fn send(&self, cmd: MediaCmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn snapshot(&self) -> NowPlaying {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Lyrics state for the current track. The first call for a new track starts a
    /// background lookup; later calls just return the current status.
    pub fn lyrics_for(&self, np: &NowPlaying) -> LyricsStatus {
        if np.title.is_empty() {
            return LyricsStatus::Idle;
        }
        let key = format!("{}\u{1}{}", np.title, np.artist);
        let Ok(mut slot) = self.lyrics.lock() else {
            return LyricsStatus::Idle;
        };
        if slot.key != key {
            slot.key = key.clone();
            slot.status = LyricsStatus::Loading;
            let (title, artist, dur_s) = (np.title.clone(), np.artist.clone(), np.dur_ms / 1000);
            let shared = self.lyrics.clone();
            thread::spawn(move || {
                let result = fetch_lyrics(&title, &artist, dur_s);
                if let Ok(mut s) = shared.lock() {
                    // Ignore the answer if the song changed while we were searching.
                    if s.key == key {
                        s.status = result;
                    }
                }
            });
        }
        slot.status.clone()
    }

    /// Forget a failed / empty lookup so the next `lyrics_for` tries again.
    pub fn retry_lyrics(&self) {
        if let Ok(mut slot) = self.lyrics.lock() {
            if matches!(slot.status, LyricsStatus::Failed | LyricsStatus::NotFound) {
                slot.key.clear();
                slot.status = LyricsStatus::Idle;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Lyrics lookup (LRCLIB)
// ---------------------------------------------------------------------------

fn http_get(url: &str) -> Option<String> {
    use std::process::Command;
    let mut cmd = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    cmd.args([
        "-sSf",
        "-L",
        "--connect-timeout",
        "4",
        "--max-time",
        "6",
        "-A",
        "winnotch/0.1 (personal notch app)",
        url,
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Picks the best lyrics record from an LRCLIB search response (synced > plain >
/// instrumental; among equals, the one whose length is closest to the track's).
fn parse_results(body: &str, dur_s: u64) -> Option<Lyrics> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut best: Option<(i64, Lyrics)> = None;
    for item in v.as_array()? {
        let synced = item
            .get("syncedLyrics")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let plain = item
            .get("plainLyrics")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let instrumental = item
            .get("instrumental")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let item_dur = item.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0);

        let lyrics = if !synced.trim().is_empty() {
            let lines = parse_lrc(synced);
            if lines.is_empty() {
                continue;
            }
            Lyrics {
                lines,
                synced: true,
                instrumental: false,
            }
        } else if !plain.trim().is_empty() {
            Lyrics {
                lines: plain_lines(plain),
                synced: false,
                instrumental: false,
            }
        } else if instrumental {
            Lyrics {
                lines: Vec::new(),
                synced: false,
                instrumental: true,
            }
        } else {
            continue;
        };

        let mut score: i64 = if lyrics.synced {
            0
        } else if lyrics.instrumental {
            2000
        } else {
            1000
        };
        if dur_s > 0 && item_dur > 0.0 {
            score += (item_dur - dur_s as f64).abs() as i64;
        }
        if best.as_ref().map_or(true, |(s, _)| score < *s) {
            best = Some((score, lyrics));
        }
    }
    best.map(|(_, l)| l)
}

fn fetch_lyrics(title: &str, artist: &str, dur_s: u64) -> LyricsStatus {
    let clean = clean_title(title);
    let artist_c = artist
        .trim()
        .trim_end_matches(" - Topic")
        .trim()
        .to_string();
    let base = "https://lrclib.net/api/search";

    let mut urls: Vec<String> = Vec::new();
    // YouTube-style "Artist - Song" titles: the real artist is in the title.
    if let Some((a, t)) = clean.split_once(" - ") {
        urls.push(format!(
            "{base}?track_name={}&artist_name={}",
            url_encode(t.trim()),
            url_encode(a.trim())
        ));
    }
    if !artist_c.is_empty() {
        urls.push(format!(
            "{base}?track_name={}&artist_name={}",
            url_encode(&clean),
            url_encode(&artist_c)
        ));
        urls.push(format!(
            "{base}?q={}",
            url_encode(&format!("{artist_c} {clean}"))
        ));
    }
    urls.push(format!("{base}?q={}", url_encode(&clean)));

    // Run every query at once; answer as soon as the highest-priority query that
    // has a result is known (so a slow low-priority query never holds us up).
    let n = urls.len();
    let (tx, rx) = mpsc::channel::<(usize, bool, Option<Lyrics>)>();
    for (i, url) in urls.into_iter().enumerate() {
        let tx = tx.clone();
        thread::spawn(move || {
            let body = http_get(&url);
            let reached = body.is_some();
            let found = body.and_then(|b| parse_results(&b, dur_s));
            let _ = tx.send((i, reached, found));
        });
    }
    drop(tx);
    let mut slots: Vec<Option<Option<Lyrics>>> = (0..n).map(|_| None).collect();
    let mut any_reached = false;
    for (i, reached, found) in rx {
        any_reached |= reached;
        slots[i] = Some(found);
        for idx in 0..n {
            match &slots[idx] {
                None => break,
                Some(None) => continue,
                Some(Some(_)) => {}
            }
            if let Some(Some(l)) = slots[idx].take() {
                return LyricsStatus::Found(Arc::new(l));
            }
        }
    }
    if any_reached {
        LyricsStatus::NotFound
    } else {
        LyricsStatus::Failed
    }
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

#[cfg(windows)]
fn fetch_art(
    props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties,
) -> Option<ArtImage> {
    use windows::Storage::Streams::DataReader;

    let thumb = props.Thumbnail().ok()?;
    let stream = thumb.OpenReadAsync().ok()?.get().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 {
        return None;
    }
    let input = stream.GetInputStreamAt(0).ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    reader.LoadAsync(size).ok()?.get().ok()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes).ok()?;

    let img = image::load_from_memory(&bytes)
        .ok()?
        .thumbnail(160, 160)
        .to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    Some(ArtImage {
        w,
        h,
        rgba: img.into_raw(),
    })
}

/// Prefer a session that is actually playing; otherwise Windows' "current" one.
#[cfg(windows)]
fn pick_session(manager: &Manager) -> Option<Session> {
    let mut first = None;
    if let Ok(list) = manager.GetSessions() {
        for s in list {
            if let Ok(info) = s.GetPlaybackInfo() {
                if info
                    .PlaybackStatus()
                    .map(|st| st == Status::Playing)
                    .unwrap_or(false)
                {
                    return Some(s);
                }
            }
            if first.is_none() {
                first = Some(s);
            }
        }
    }
    manager.GetCurrentSession().ok().or(first)
}

/// 100-ns ticks since 1601-01-01 (the epoch of Windows' DateTime).
#[cfg(windows)]
fn now_ticks() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_nanos() / 100) as i64 + 116_444_736_000_000_000
}

/// Finds a VLC window and returns (artist, title) parsed from its title bar.
#[cfg(windows)]
fn vlc_track() -> Option<(String, String)> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, IsWindowVisible};

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<String>);
        if IsWindowVisible(hwnd).as_bool() {
            let mut buf = [0u16; 512];
            let n = GetWindowTextW(hwnd, &mut buf);
            if n > 0 {
                out.push(String::from_utf16_lossy(&buf[..n as usize]));
            }
        }
        BOOL(1)
    }

    let mut titles: Vec<String> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut titles as *mut Vec<String> as isize));
    }
    for t in titles {
        if let Some(rest) = t.strip_suffix(" - VLC media player") {
            let rest = rest.trim();
            if rest.is_empty() {
                continue;
            }
            return Some(match rest.split_once(" - ") {
                Some((artist, title)) => (artist.to_string(), title.to_string()),
                None => (String::new(), rest.to_string()),
            });
        }
    }
    None
}

#[cfg(windows)]
fn send_media_key(cmd: MediaCmd) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    };
    let vk: u8 = match cmd {
        MediaCmd::PlayPause => 0xB3, // VK_MEDIA_PLAY_PAUSE
        MediaCmd::Next => 0xB0,      // VK_MEDIA_NEXT_TRACK
        MediaCmd::Prev => 0xB1,      // VK_MEDIA_PREV_TRACK
        MediaCmd::Seek(_) => return, // media keys can't seek
    };
    unsafe {
        keybd_event(vk, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
    }
}

#[cfg(windows)]
fn worker(state: Arc<Mutex<NowPlaying>>, rx: mpsc::Receiver<MediaCmd>) {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let manager = match Manager::RequestAsync().and_then(|op| op.get()) {
        Ok(m) => m,
        Err(_) => return,
    };

    let mut last_key = String::new();
    let mut art: Option<Arc<ArtImage>> = None;
    let mut art_id: u64 = 0;
    let mut art_tries = 0;

    loop {
        let cmds: Vec<MediaCmd> = rx.try_iter().collect();
        let session = pick_session(&manager);

        let mut np = NowPlaying::default();
        let mut props = None;

        if let Some(session) = &session {
            props = session
                .TryGetMediaPropertiesAsync()
                .and_then(|o| o.get())
                .ok();
            if let Some(pr) = &props {
                np.title = pr.Title().map(|s| s.to_string()).unwrap_or_default();
                np.artist = pr.Artist().map(|s| s.to_string()).unwrap_or_default();
                np.album = pr.AlbumTitle().map(|s| s.to_string()).unwrap_or_default();
            }
            if let Ok(info) = session.GetPlaybackInfo() {
                np.playing = info
                    .PlaybackStatus()
                    .map(|s| s == Status::Playing)
                    .unwrap_or(false);
            }
            // Position / length. Players report the position as of `LastUpdatedTime`,
            // so while playing we advance it by the time elapsed since then.
            if let Ok(tl) = session.GetTimelineProperties() {
                let start = tl.StartTime().map(|t| t.Duration).unwrap_or(0);
                let end = tl.EndTime().map(|t| t.Duration).unwrap_or(0);
                let pos = tl.Position().map(|t| t.Duration).unwrap_or(0);
                let updated = tl.LastUpdatedTime().map(|t| t.UniversalTime).unwrap_or(0);
                let len = end - start;
                if len > 0 {
                    let mut p = pos - start;
                    if np.playing && updated > 0 {
                        p += (now_ticks() - updated).max(0);
                    }
                    np.dur_ms = (len / 10_000) as u64;
                    np.pos_ms = ((p.max(0) / 10_000) as u64).min(np.dur_ms);
                    np.stamp = Some(Instant::now());
                }
            }
        }

        // A paused SMTC session (e.g. Spotify) shouldn't hide VLC: only trust SMTC
        // if it's actually playing, or if VLC has nothing loaded.
        let vlc = if np.playing && !np.title.is_empty() {
            None
        } else {
            vlc_track()
        };
        let smtc_ok = !np.title.is_empty() && vlc.is_none();

        if smtc_ok {
            if let Some(session) = &session {
                for cmd in cmds {
                    let _ = match cmd {
                        MediaCmd::PlayPause => {
                            session.TryTogglePlayPauseAsync().and_then(|o| o.get())
                        }
                        MediaCmd::Next => session.TrySkipNextAsync().and_then(|o| o.get()),
                        MediaCmd::Prev => session.TrySkipPreviousAsync().and_then(|o| o.get()),
                        MediaCmd::Seek(ms) => session
                            .TryChangePlaybackPositionAsync(ms as i64 * 10_000)
                            .and_then(|o| o.get()),
                    };
                }
            }
        } else {
            // No usable SMTC session -> try VLC, and drive it with media keys.
            np = NowPlaying::default();
            props = None;
            for cmd in cmds {
                send_media_key(cmd);
            }
            if let Some((artist, title)) = vlc {
                np.title = title;
                np.artist = artist;
                np.playing = true; // VLC's title doesn't tell us if it's paused
            }
        }

        // New track -> reset art; retry fetching a few times (some apps publish
        // the thumbnail slightly after the title).
        let key = format!("{}\u{1}{}", np.title, np.artist);
        if key != last_key {
            last_key = key;
            art = None;
            art_tries = 0;
            art_id += 1;
        }
        if smtc_ok && art.is_none() && art_tries < 4 {
            art_tries += 1;
            if let Some(a) = props.as_ref().and_then(fetch_art) {
                art = Some(Arc::new(a));
                art_id += 1;
            }
        }

        np.art = art.clone();
        np.art_id = art_id;
        if let Ok(mut s) = state.lock() {
            *s = np;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

// ---------------------------------------------------------------------------
// Non-Windows stub (UI development only)
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
fn worker(state: Arc<Mutex<NowPlaying>>, rx: mpsc::Receiver<MediaCmd>) {
    if let Ok(mut s) = state.lock() {
        *s = NowPlaying {
            title: "The Eliminating Angel".into(),
            artist: "Metavoid".into(),
            album: "Lustmord".into(),
            playing: true,
            art: None,
            art_id: 0,
            pos_ms: 50_000,
            dur_ms: 200_000,
            stamp: Some(Instant::now()),
        };
    }
    while rx.recv().is_ok() {}
}
