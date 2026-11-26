//! Preview playback: video frames and the playback clock.
//!
//! Design (documented in `docs/playback.md`):
//! * The plugin owns playback. The Pigeon host has no media service, so frames come from an
//!   `ffmpeg` child decoding the engine's loopback preview URL to raw RGBA on stdout.
//! * Frames are paced against a wall clock (`speed` scales it) and published with `resource.put`
//!   through the SDK's file ring (`frame:current`). The ring acknowledgement is honoured by the SDK
//!   writer; a put blocks only on a worker thread.
//! * Paused seeks decode one exact frame. Seeks while playing restart the decoder at the new time.
//!   Intermediate seeks are coalesced upstream (80 ms `SeekScheduler`); the final position always wins.
//! * Audio is a separate `ffplay` child with the same start position, volume and `atempo` speed.
//!   Speeds outside atempo's 0.5..2 range are rendered silent and the reducer shows the warning.
//! * The pump reports the media clock back (`PlaybackClock`), so the timelines follow playback.

use pigeon_plugin_sdk::runtime::Ctx;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Nominal decode rate for paced playback; VFR sources are approximated (documented).
pub const NOMINAL_FPS: f64 = 25.0;
pub const FRAME_RID: &str = "frame:current";

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Load {
        url: String,
        width: u32,
        height: u32,
    },
    Transport {
        playing: bool,
        from_ms: f64,
        speed: f64,
        volume: f64,
    },
    Seek {
        ms: f64,
    },
}

/// Builds the ffmpeg frame-decoder arguments. Pure, so it is unit tested.
pub fn decoder_args(
    url: &str,
    start_ms: f64,
    width: u32,
    height: u32,
    single: bool,
) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        format!("{:.3}", (start_ms.max(0.0)) / 1000.0),
        "-i".into(),
        url.into(),
        "-an".into(),
    ];
    let filter = if single {
        format!("scale={width}:{height}")
    } else {
        format!("fps={NOMINAL_FPS},scale={width}:{height}")
    };
    args.extend(["-vf".into(), filter]);
    if single {
        args.extend(["-frames:v".into(), "1".into()]);
    }
    args.extend([
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "-".into(),
    ]);
    args
}

/// Arguments for the audio child. `atempo` only accepts 0.5..2; `None` means silent.
pub fn audio_args(url: &str, start_ms: f64, speed: f64, volume: f64) -> Option<Vec<String>> {
    if !(0.5..=2.0).contains(&speed) || volume <= 0.0 {
        return None;
    }
    Some(vec![
        "-nodisp".into(),
        "-autoexit".into(),
        "-loglevel".into(),
        "quiet".into(),
        "-ss".into(),
        format!("{:.3}", start_ms.max(0.0) / 1000.0),
        "-volume".into(),
        format!("{}", (volume * 100.0).round() as i64),
        "-af".into(),
        format!("atempo={speed}"),
        "-i".into(),
        url.into(),
    ])
}

struct Frame {
    gen: u64,
    media_ms: f64,
    pixels: Vec<u8>,
}

/// Handle to the playback thread.
#[derive(Clone)]
pub struct Player {
    tx: Sender<Cmd>,
}

pub type ClockSink = Arc<dyn Fn(f64) + Send + Sync>;

impl Player {
    /// Start the playback thread. `on_clock` receives media milliseconds for every shown frame.
    pub fn start(ctx: Ctx, on_clock: ClockSink) -> Player {
        let (tx, rx) = mpsc::channel();
        let ffmpeg = std::env::var("REELIO_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        let ffplay = std::env::var("REELIO_FFPLAY").unwrap_or_else(|_| "ffplay".into());
        let mut pump = Pump {
            ctx,
            on_clock,
            ffmpeg,
            ffplay,
            url: None,
            size: (640, 360),
            gen: 0,
            latest_seek: Arc::new(AtomicU64::new(0)),
            playing: false,
            from_ms: 0.0,
            speed: 1.0,
            volume: 0.05,
            t0: Instant::now(),
            decoder: None,
            frames: None,
            audio: None,
            pending: None,
        };
        thread::spawn(move || pump.run(rx));
        Player { tx }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

struct Decoder {
    child: Child,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Pump {
    ctx: Ctx,
    on_clock: ClockSink,
    ffmpeg: String,
    ffplay: String,
    url: Option<String>,
    size: (u32, u32),
    gen: u64,
    latest_seek: Arc<AtomicU64>,
    playing: bool,
    from_ms: f64,
    speed: f64,
    volume: f64,
    t0: Instant,
    decoder: Option<Decoder>,
    frames: Option<Receiver<Frame>>,
    audio: Option<Child>,
    pending: Option<Frame>,
}

impl Pump {
    fn run(&mut self, rx: Receiver<Cmd>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(4)) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.pace();
        }
        self.stop_decoder();
        self.stop_audio();
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Load { url, width, height } => {
                self.stop_decoder();
                self.stop_audio();
                self.url = Some(url);
                self.size = (width, height);
                self.playing = false;
                let seek_id = self.latest_seek.fetch_add(1, Ordering::SeqCst) + 1;
                self.single_frame(0., seek_id);
            }
            Cmd::Transport {
                playing,
                from_ms,
                speed,
                volume,
            } => {
                self.speed = speed;
                self.volume = volume;
                self.from_ms = from_ms;
                if playing {
                    self.start_playing(from_ms);
                } else {
                    self.playing = false;
                    self.stop_decoder();
                    self.stop_audio();
                }
            }
            Cmd::Seek { ms } => {
                let seek_id = self.latest_seek.fetch_add(1, Ordering::SeqCst) + 1;
                if self.playing {
                    self.start_playing(ms);
                } else {
                    self.stop_decoder();
                    self.stop_audio();
                    self.from_ms = ms;
                    self.single_frame(ms, seek_id);
                }
            }
        }
    }

    fn start_playing(&mut self, from_ms: f64) {
        self.stop_decoder();
        self.stop_audio();
        let Some(url) = self.url.clone() else { return };
        self.gen += 1;
        self.from_ms = from_ms;
        self.t0 = Instant::now();
        self.playing = true;
        let args = decoder_args(&url, from_ms, self.size.0, self.size.1, false);
        let Ok(mut child) = Command::new(&self.ffmpeg)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            self.playing = false;
            return;
        };
        let Some(mut stdout) = child.stdout.take() else {
            self.playing = false;
            return;
        };
        let (tx, rx): (SyncSender<Frame>, Receiver<Frame>) = mpsc::sync_channel(3);
        let gen = self.gen;
        let frame_len = self.size.0 as usize * self.size.1 as usize * 4;
        let start = from_ms;
        thread::spawn(move || {
            let mut index = 0u64;
            loop {
                let mut pixels = vec![0u8; frame_len];
                if stdout.read_exact(&mut pixels).is_err() {
                    break;
                }
                let media_ms = start + index as f64 * 1000.0 / NOMINAL_FPS;
                index += 1;
                if tx
                    .send(Frame {
                        gen,
                        media_ms,
                        pixels,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.decoder = Some(Decoder { child });
        self.frames = Some(rx);
        if let Some(args) = audio_args(&url, from_ms, self.speed, self.volume) {
            self.audio = Command::new(&self.ffplay)
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok();
        }
    }

    /// Show the newest frame whose time has come; late frames are dropped to keep up.
    fn pace(&mut self) {
        if !self.playing {
            return;
        }
        let Some(rx) = self.frames.as_ref() else {
            return;
        };
        let elapsed = self.t0.elapsed().as_secs_f64() * 1000.0 * self.speed;
        loop {
            match rx.try_recv() {
                Ok(frame) => self.pending = Some(frame),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.frames = None;
                    self.playing = false;
                    self.stop_audio();
                    break;
                }
            }
        }
        if let Some(frame) = self.pending.take() {
            let due_ms = frame.media_ms - self.from_ms;
            if frame.gen != self.gen {
                return;
            }
            if due_ms <= elapsed {
                self.publish(frame.pixels, frame.media_ms);
            } else {
                self.pending = Some(frame);
            }
        }
    }

    fn single_frame(&mut self, ms: f64, seek_id: u64) {
        let Some(url) = self.url.clone() else { return };
        let args = decoder_args(&url, ms, self.size.0, self.size.1, true);
        let (w, h) = self.size;
        let ffmpeg = self.ffmpeg.clone();
        let latest = self.latest_seek.clone();
        let ctx = self.ctx.clone();
        let on_clock = self.on_clock.clone();
        thread::spawn(move || {
            let Ok(out) = Command::new(&ffmpeg)
                .args(&args)
                .stderr(Stdio::null())
                .output()
            else {
                return;
            };
            // Only the newest seek may publish: an older decode result is simply dropped.
            if latest.load(Ordering::SeqCst) != seek_id
                || out.stdout.len() != w as usize * h as usize * 4
            {
                return;
            }
            if ctx
                .resources()
                .put_rgba(FRAME_RID, w, h, &out.stdout, false)
                .is_ok()
            {
                on_clock(ms);
            }
        });
    }

    fn publish(&mut self, pixels: Vec<u8>, media_ms: f64) {
        let (w, h) = self.size;
        // The SDK's writer blocks until the host acknowledges the ring slot; it runs off this thread.
        let resources = self.ctx.resources();
        let on_clock = self.on_clock.clone();
        thread::spawn(move || {
            if resources.put_rgba(FRAME_RID, w, h, &pixels, false).is_ok() {
                on_clock(media_ms);
            }
        });
    }

    fn stop_decoder(&mut self) {
        self.decoder = None;
        self.frames = None;
        self.pending = None;
    }

    fn stop_audio(&mut self) {
        if let Some(mut child) = self.audio.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_arguments_seek_before_input_and_pipe_rgba() {
        let args = decoder_args("http://127.0.0.1:1/x", 1500.0, 320, 180, false);
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < input, "fast seek before the input");
        assert_eq!(args[ss + 1], "1.500");
        assert!(args.iter().any(|a| a == "fps=25,scale=320:180"));
        assert!(args.windows(2).any(|w| w == ["-pix_fmt", "rgba"]));
        assert_eq!(args.last().unwrap(), "-");
    }

    #[test]
    fn single_frame_decode_requests_exactly_one_frame() {
        let args = decoder_args("u", 0.0, 2, 2, true);
        assert!(args.windows(2).any(|w| w == ["-frames:v", "1"]));
    }

    #[test]
    fn audio_follows_speed_within_atempo_range_only() {
        assert!(audio_args("u", 0.0, 1.0, 0.05).is_some());
        assert!(
            audio_args("u", 0.0, 0.1, 0.05).is_none(),
            "silent outside atempo range"
        );
        assert!(audio_args("u", 0.0, 10.0, 0.05).is_none());
        assert!(
            audio_args("u", 0.0, 1.0, 0.0).is_none(),
            "muted at zero volume"
        );
        let args = audio_args("u", 2000.0, 2.0, 0.5).unwrap();
        assert!(args.windows(2).any(|w| w == ["-af", "atempo=2"]));
        assert!(args.windows(2).any(|w| w == ["-volume", "50"]));
    }
}
