//! Playback for YouTube Music: what [`crate::engine`] needs that is YouTube's own.
//!
//! The stream host drops connections halfway and has no timeout of its own, so the whole track
//! is downloaded under a deadline before it plays, and a stalled download gets one more go. The
//! threads, the queue, the preload, the gapless join and the loudness gain are the engine's.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use rodio::Source as _;
use ytmusic::YtMusic;

use crate::audio::Trimmed;
use crate::engine::{self, Fetch, Loudness};
use crate::trim;
use crate::{PlaybackConfig, PlaybackEvents, PlaybackFactory, Player};

/// How long the stream metadata, or a download with nothing known about its size, may take
/// before the attempt is given up. The stream host has no timeout of its own, so a
/// connection it dropped halfway would otherwise hold the engine on a silent track forever.
const PATIENCE: Duration = Duration::from_secs(15);
/// What every mebibyte of a download adds to `PATIENCE`. A link slower than that is not one
/// the track would play over anyway.
const PER_MIB: Duration = Duration::from_secs(4);
/// The size a download is budgeted at when the stream host announced none.
const UNSIZED_MIB: u64 = 8;
/// The attempts a fetch gets before the track is reported unavailable.
const ATTEMPTS: u32 = 2;
/// The level the stream host measures `loudnessDb` against, in LUFS.
const REFERENCE_LUFS: f32 = -14.0;

/// A track downloaded in full, with what the stream host said about its length and loudness.
#[derive(Clone)]
pub struct Loaded {
    data: Arc<Vec<u8>>,
    loudness_db: Option<f32>,
    duration: Option<Duration>,
}

pub struct Factory {
    api: Arc<YtMusic>,
}

impl Factory {
    pub fn new(api: Arc<YtMusic>) -> Self {
        Self { api }
    }
}

impl PlaybackFactory for Factory {
    fn start(&self, config: PlaybackConfig) -> (Box<dyn Player>, Box<dyn PlaybackEvents>) {
        engine::start(
            YouTube {
                api: self.api.clone(),
            },
            config,
        )
    }
}

struct YouTube {
    api: Arc<YtMusic>,
}

#[async_trait]
impl Fetch for YouTube {
    type Loaded = Loaded;
    type Source = Trimmed<rodio::Decoder<Cursor<Bytes>>>;

    fn name(&self) -> &'static str {
        "yt"
    }

    /// Downloads the track, giving a stalled attempt one more go before failing. Only a
    /// timeout is retried, since a refusal from the stream host is as final the second time.
    async fn load(&self, id: &str) -> Result<Loaded> {
        let mut attempt = 1;
        loop {
            match attempt_fetch(&self.api, id).await {
                Err(error) if attempt < ATTEMPTS && error.is::<tokio::time::error::Elapsed>() => {
                    log::warn!("playback: {id} stalled, trying again: {error:#}");
                    attempt += 1;
                }
                result => return result,
            }
        }
    }

    fn length(&self, loaded: &Loaded) -> Option<Duration> {
        loaded.duration
    }

    fn loudness(&self, loaded: &Loaded) -> Option<Loudness> {
        loaded.loudness_db.map(|db| Loudness {
            lufs: REFERENCE_LUFS + db,
            peak: None,
        })
    }

    fn gated(&self, error: &anyhow::Error) -> bool {
        error.downcast_ref::<ytmusic::SignInRequired>().is_some()
    }

    /// Builds a decoder over the download, trimmed to what the edit list says is heard, and
    /// places it at `at`.
    fn open(&self, id: &str, loaded: &Loaded, at: Duration) -> Option<Self::Source> {
        let length = loaded.data.len() as u64;
        let decoder = match rodio::Decoder::builder()
            .with_data(Cursor::new(Bytes(loaded.data.clone())))
            .with_byte_len(length)
            .with_seekable(true)
            .build()
        {
            Ok(decoder) => decoder,
            Err(error) => {
                log::warn!("playback: cannot decode the youtube track {id}: {error}");
                return None;
            }
        };
        let edit = trim::from_mp4(&loaded.data);
        match edit {
            Some(edit) => log::debug!(
                "playback: {id} trims {:?} of priming, plays {:?}",
                edit.skip,
                edit.take
            ),
            None => log::debug!("playback: {id} carries no edit list"),
        }
        let mut source = Trimmed::new(
            decoder,
            edit.map(|edit| edit.skip).unwrap_or_default(),
            edit.and_then(|edit| edit.take),
        );
        if !at.is_zero()
            && let Err(error) = source.try_seek(at)
        {
            log::warn!("playback: cannot start {id} at {}s: {error}", at.as_secs());
        }
        Some(source)
    }
}

/// A download shared between every decoder opened over it.
struct Bytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

async fn attempt_fetch(api: &YtMusic, id: &str) -> Result<Loaded> {
    let started = std::time::Instant::now();
    let format = tokio::time::timeout(PATIENCE, api.best_audio(id))
        .await
        .context("stream metadata timed out")??;
    let data = tokio::time::timeout(allowance(format.content_length), api.download(&format))
        .await
        .context("stream download timed out")??;
    log::debug!(
        "playback: {id} loaded, itag {} {} {} kbps, {:.1} MiB in {:?}",
        format.itag,
        format.codec,
        format.bitrate / 1000,
        data.len() as f64 / (1024.0 * 1024.0),
        started.elapsed()
    );
    Ok(Loaded {
        data: Arc::new(data),
        loudness_db: format.loudness_db,
        duration: format.duration,
    })
}

/// How long a download of `bytes` may take: `PATIENCE` plus `PER_MIB` for every mebibyte.
/// A size the host did not announce is budgeted as a long track, `UNSIZED_MIB`.
fn allowance(bytes: Option<u64>) -> Duration {
    let mib = bytes.map_or(UNSIZED_MIB, |bytes| bytes.div_ceil(1024 * 1024));
    PATIENCE + PER_MIB * mib as u32
}
