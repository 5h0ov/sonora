use std::ffi::c_void;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use music::Track;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use tokio::sync::mpsc;

use crate::{Cover, Io, Playback, PlaybackState, Sonora, join};

const BUS_NAME: &str = "sonora";
const DISPLAY_NAME: &str = "Sonora";
const SEEK_STEP: Duration = Duration::from_secs(5);
const ARTWORK: &str = "artwork";

struct Attached {
    _remote: Entity<Remote>,
}

impl Global for Attached {}

pub fn attach(hwnd: Option<*mut c_void>, cx: &mut App) {
    if cx.has_global::<Attached>() {
        return;
    }
    let config = PlatformConfig {
        dbus_name: BUS_NAME,
        display_name: DISPLAY_NAME,
        hwnd,
    };
    let controls = match MediaControls::new(config) {
        Ok(controls) => controls,
        Err(error) => {
            return log::warn!("remote: cannot reach the system media controls: {error:?}");
        }
    };

    let sonora = Sonora::global(cx);
    let playback = sonora.playback.clone();
    let cover = sonora.cover.clone();
    let io = Io::global(cx);
    let remote = cx.new(|cx| Remote::new(controls, playback, cover, io, cx));
    cx.set_global(Attached { _remote: remote });
}

pub struct Remote {
    controls: MediaControls,
    playback: Entity<Playback>,
    cover: Entity<Cover>,
    io: Io,
    shown: Option<String>,
    source: Option<String>,
    reported: Option<PlaybackState>,
    at: Duration,
    artwork: Option<Task<()>>,
    _events: Task<()>,
}

impl Remote {
    fn new(
        mut controls: MediaControls,
        playback: Entity<Playback>,
        cover: Entity<Cover>,
        io: Io,
        cx: &mut Context<Self>,
    ) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        if let Err(error) = controls.attach(move |event| {
            sender.send(event).ok();
        }) {
            log::warn!("remote: cannot listen for media keys: {error:?}");
        }

        let _events = cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.recv().await {
                if this.update(cx, |this, cx| this.act(event, cx)).is_err() {
                    break;
                }
            }
        });

        cx.observe(&playback, |this, _, cx| this.publish(cx))
            .detach();
        // the album art resolves after the track it belongs to, so republish when it lands
        cx.observe(&cover, |this, _, cx| this.publish(cx)).detach();

        Self {
            controls,
            playback,
            cover,
            io,
            shown: None,
            source: None,
            reported: None,
            at: Duration::ZERO,
            artwork: None,
            _events,
        }
    }

    fn act(&mut self, event: MediaControlEvent, cx: &mut Context<Self>) {
        self.playback
            .clone()
            .update(cx, |playback, cx| match event {
                MediaControlEvent::Play => playback.resume(cx),
                MediaControlEvent::Pause | MediaControlEvent::Stop => playback.pause(cx),
                MediaControlEvent::Toggle => playback.toggle_play(cx),
                MediaControlEvent::Next => playback.next(cx),
                MediaControlEvent::Previous => playback.previous(cx),
                MediaControlEvent::SetPosition(MediaPosition(at)) => playback.seek(at, cx),
                MediaControlEvent::Seek(direction) => shift(playback, direction, SEEK_STEP, cx),
                MediaControlEvent::SeekBy(direction, step) => shift(playback, direction, step, cx),
                MediaControlEvent::SetVolume(level) => playback.set_volume(level as f32, cx),
                MediaControlEvent::OpenUri(_)
                | MediaControlEvent::Raise
                | MediaControlEvent::Quit => {}
            });
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        let playback = self.playback.read(cx);
        let state = playback.state().clone();
        let at = playback.position();
        let track = playback.track().cloned();

        let id = track.as_ref().and_then(|track| track.id.clone());
        let cover = track.as_ref().and_then(|track| self.artwork_url(track, cx));
        let moved = id != self.shown;
        if moved || cover != self.source {
            self.shown = id;
            self.source = cover.clone();
            self.artwork = None;
            let remote = cover.as_deref().is_some_and(is_remote);
            match (moved, remote) {
                // a remote cover follows once it sits in the cache, and a sharper one for the
                // track already on show leaves the published thumbnail up until the file lands
                (false, true) => {}
                // anything else is a file already
                _ => self.describe(track.as_ref(), cover.as_deref().filter(|_| !remote)),
            }
            if let (Some(track), Some(url), true) = (track, cover, remote) {
                self.artwork = Some(self.fetch_artwork(track, url, cx));
            }
        }

        if self.reported.as_ref() == Some(&state) && self.at.as_secs() == at.as_secs() {
            return;
        }
        self.reported = Some(state.clone());
        self.at = at;

        let progress = Some(MediaPosition(at));
        let reported = match state {
            PlaybackState::Playing | PlaybackState::Loading => MediaPlayback::Playing { progress },
            PlaybackState::Paused => MediaPlayback::Paused { progress },
            PlaybackState::Idle | PlaybackState::Failed(_) => MediaPlayback::Stopped,
        };
        if let Err(error) = self.controls.set_playback(reported) {
            log::warn!("remote: cannot publish playback state: {error:?}");
        }
    }
}

impl Remote {
    /// The cover to publish: the album art `Cover` resolves once it arrives, the thumbnail the
    /// track carries until then. Spotify ships a 64px thumbnail with a track, which is plenty
    /// for a list row and blurry in a desktop widget that draws it several times that size.
    fn artwork_url(&self, track: &Track, cx: &App) -> Option<String> {
        track
            .album_id
            .as_deref()
            .and_then(|album| self.cover.read(cx).large_for(album))
            .map(str::to_owned)
            .or_else(|| track.cover.clone())
    }

    fn describe(&mut self, track: Option<&Track>, cover: Option<&str>) {
        let metadata = match track {
            Some(track) => MediaMetadata {
                title: Some(&track.name),
                artist: Some(&track.artists),
                album: Some(&track.album),
                duration: Some(track.duration),
                cover_url: cover,
            },
            None => MediaMetadata::default(),
        };
        if let Err(error) = self.controls.set_metadata(metadata) {
            log::warn!("remote: cannot publish the current track: {error:?}");
        }
    }

    /// Brings a remote cover into the cache and republishes the track with the file. The
    /// platform widget would otherwise download it itself, on its own thread, and on macOS a
    /// download that fails there takes the process down.
    fn fetch_artwork(&self, track: Track, url: String, cx: &mut Context<Self>) -> Task<()> {
        let io = self.io.clone();
        cx.spawn(async move |this, cx| {
            let fetched = join(io.spawn(async move { artwork(&url).await })).await;
            let path = match fetched {
                Ok(path) => path,
                Err(error) => return log::debug!("remote: cannot fetch the cover: {error:#}"),
            };
            this.update(cx, |this, _| {
                if this.shown == track.id {
                    this.describe(Some(&track), Some(&format!("file://{}", path.display())));
                }
            })
            .ok();
        })
    }
}

fn is_remote(cover: &str) -> bool {
    cover.starts_with("http://") || cover.starts_with("https://")
}

/// The cached file for a cover url, downloaded on first sight. The bytes have to decode as an
/// image before they are kept, so the widget never opens something it cannot draw.
async fn artwork(url: &str) -> Result<PathBuf> {
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sonora")
        .join(ARTWORK);
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let key = format!("{:016x}", hasher.finish());
    for format in [
        image::ImageFormat::Jpeg,
        image::ImageFormat::Png,
        image::ImageFormat::WebP,
    ] {
        let candidate = dir.join(&key).with_extension(extension(format));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    let bytes = reqwest::get(url)
        .await
        .context("cannot request the cover")?
        .error_for_status()
        .context("the cover request was refused")?
        .bytes()
        .await
        .context("cannot read the cover")?;
    let format = image::guess_format(&bytes).context("cannot tell the cover format")?;
    image::load_from_memory_with_format(&bytes, format).context("cannot decode the cover")?;

    std::fs::create_dir_all(&dir).context("cannot create the artwork cache")?;
    let path = dir.join(&key).with_extension(extension(format));
    std::fs::write(&path, &bytes).context("cannot store the cover")?;
    Ok(path)
}

fn extension(format: image::ImageFormat) -> &'static str {
    format.extensions_str().first().copied().unwrap_or("img")
}

fn shift(
    playback: &mut Playback,
    direction: SeekDirection,
    step: Duration,
    cx: &mut Context<Playback>,
) {
    let at = playback.position();
    let target = match direction {
        SeekDirection::Forward => at.saturating_add(step),
        SeekDirection::Backward => at.saturating_sub(step),
    };
    let end = playback
        .track()
        .map(|track| track.duration)
        .unwrap_or(target);
    playback.seek(target.min(end), cx);
}
