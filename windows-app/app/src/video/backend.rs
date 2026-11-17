//! Where players come from: Media Foundation in the app (`MfBackend`), a scripted stand-in in the
//! headless UI tests (`FakeBackend`), which have neither a decoder they could rely on nor the
//! time to wait for one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use instagram_core::video_rules::PlaybackFailure;

use super::player::{Command, Phase, PlayerOptions, PlayerStatus, VideoFrame, VideoPlayer, Waker};

/// One open player, as the hub sees it.
pub trait PlayerHandle {
    /// Sends a command; never blocks.
    fn send(&self, command: Command);
    /// The state now.
    fn status(&self) -> PlayerStatus;
    /// The newest frame newer than `after`.
    fn take_frame(&self, after: u64) -> Option<VideoFrame>;
}

/// Opens players.
pub trait VideoBackend {
    /// A player for the video at `path`; `Err` holds why no player could be started (a thread
    /// could not be made).
    fn open(&self, path: &Path, options: PlayerOptions, waker: Waker) -> Result<Box<dyn PlayerHandle>, String>;
}

impl PlayerHandle for VideoPlayer {
    fn send(&self, command: Command) {
        VideoPlayer::send(self, command);
    }

    fn status(&self) -> PlayerStatus {
        VideoPlayer::status(self)
    }

    fn take_frame(&self, after: u64) -> Option<VideoFrame> {
        VideoPlayer::take_frame(self, after)
    }
}

/// Media Foundation's Media Engine, one thread per player.
#[derive(Clone, Copy, Debug, Default)]
pub struct MfBackend;

impl VideoBackend for MfBackend {
    fn open(&self, path: &Path, options: PlayerOptions, waker: Waker) -> Result<Box<dyn PlayerHandle>, String> {
        VideoPlayer::open(path, options, waker).map(|player| Box::new(player) as Box<dyn PlayerHandle>).map_err(|error| error.to_string())
    }
}

/// How a fake player behaves for one file (by file name).
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub struct FakeScript {
    /// The length it reports (`None`: not known).
    pub duration: Option<f64>,
    /// It fails with this.
    pub failure: Option<PlaybackFailure>,
    /// The size it reports.
    pub native_size: (u32, u32),
}

impl Default for FakeScript {
    fn default() -> FakeScript {
        FakeScript { duration: Some(10.0), failure: None, native_size: (640, 360) }
    }
}

/// What the fake backend did and is told to do. Tests read and change it.
#[doc(hidden)]
#[derive(Default)]
pub struct FakeLog {
    /// Every player opened: its file and options.
    pub opened: Vec<(PathBuf, PlayerOptions)>,
    /// Every command, with the file of its player.
    pub commands: Vec<(PathBuf, Command)>,
    /// Every player dropped, by file.
    pub closed: Vec<PathBuf>,
    /// Scripts by file name; a file without one plays 10 s of 640 x 360.
    pub scripts: HashMap<String, FakeScript>,
    /// The live players' states by player number.
    states: HashMap<u64, (PathBuf, PlayerStatus)>,
    /// The serial of the newest frame of each live player, by player number (1 when opened).
    frames: HashMap<u64, u64>,
    next: u64,
}

impl FakeLog {
    /// The live player of `file` (by file name) has one more new frame.
    pub fn push_frame(&mut self, file: &str) {
        let ids: Vec<u64> = self.states.iter().filter(|(_, (path, _))| path.file_name().is_some_and(|name| name.to_string_lossy() == file)).map(|(id, _)| *id).collect();
        for id in ids {
            *self.frames.entry(id).or_insert(1) += 1;
        }
    }

    /// The status of the live player of `file` (by file name), to change it.
    pub fn status_mut(&mut self, file: &str) -> Option<&mut PlayerStatus> {
        self.states.values_mut().find(|(path, _)| path.file_name().is_some_and(|name| name.to_string_lossy() == file)).map(|(_, status)| status)
    }

    /// The commands sent to players of `file` (by file name).
    pub fn commands_for(&self, file: &str) -> Vec<Command> {
        self.commands.iter().filter(|(path, _)| path.file_name().is_some_and(|name| name.to_string_lossy() == file)).map(|(_, command)| command.clone()).collect()
    }

    /// How many players are open.
    pub fn live(&self) -> usize {
        self.states.len()
    }
}

/// A backend whose players play nothing: they report what their script says, follow Play, Pause
/// and Seek in their status, and deliver one frame of a solid colour (more with
/// `FakeLog::push_frame`).
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct FakeBackend {
    /// Shared with the test.
    pub log: Arc<Mutex<FakeLog>>,
}

impl FakeBackend {
    /// The log; a poisoned lock is still usable.
    pub fn log(&self) -> MutexGuard<'_, FakeLog> {
        self.log.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct FakePlayer {
    id: u64,
    path: PathBuf,
    log: Arc<Mutex<FakeLog>>,
}

impl FakePlayer {
    fn log(&self) -> MutexGuard<'_, FakeLog> {
        self.log.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl VideoBackend for FakeBackend {
    fn open(&self, path: &Path, options: PlayerOptions, waker: Waker) -> Result<Box<dyn PlayerHandle>, String> {
        let mut log = self.log();
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let script = log.scripts.get(&name).cloned().unwrap_or_default();
        let status = PlayerStatus {
            phase: if script.failure.is_some() {
                Phase::Failed
            } else if options.autoplay {
                Phase::Playing
            } else {
                Phase::Paused
            },
            duration: script.duration,
            native_size: Some(script.native_size),
            has_video: script.failure.is_none(),
            error: script.failure.clone(),
            mime: options.mime.clone().unwrap_or_else(|| "video/mp4".to_owned()),
            ..PlayerStatus::default()
        };
        let id = log.next;
        log.next += 1;
        log.states.insert(id, (path.to_path_buf(), status));
        log.frames.insert(id, 1);
        log.opened.push((path.to_path_buf(), options));
        drop(log);
        waker();
        Ok(Box::new(FakePlayer { id, path: path.to_path_buf(), log: Arc::clone(&self.log) }))
    }
}

impl PlayerHandle for FakePlayer {
    fn send(&self, command: Command) {
        let mut log = self.log();
        if let Some((_, status)) = log.states.get_mut(&self.id) {
            if status.phase != Phase::Failed {
                match &command {
                    Command::Play => status.phase = Phase::Playing,
                    Command::Pause => status.phase = Phase::Paused,
                    Command::Seek(seconds, _) => status.current_time = *seconds,
                    _ => {}
                }
            }
        }
        log.commands.push((self.path.clone(), command));
    }

    fn status(&self) -> PlayerStatus {
        self.log().states.get(&self.id).map(|(_, status)| status.clone()).unwrap_or_default()
    }

    fn take_frame(&self, after: u64) -> Option<VideoFrame> {
        let status = self.status();
        let serial = self.log().frames.get(&self.id).copied().unwrap_or(1);
        if after >= serial || status.phase == Phase::Failed || status.phase == Phase::Loading {
            return None;
        }
        let (width, height) = (16, 9);
        let image = egui::ColorImage::new([width, height], vec![egui::Color32::from_rgb(200, 40, 160); width * height]);
        Some(VideoFrame { image, pts: status.current_time, serial })
    }
}

impl Drop for FakePlayer {
    fn drop(&mut self) {
        let mut log = self.log();
        log.states.remove(&self.id);
        log.frames.remove(&self.id);
        log.closed.push(self.path.clone());
    }
}
