//! The Media Engine on its own thread: every `unsafe` Media Foundation, WIC and Direct3D call of
//! playback (ported from the video spike, where each rule below was measured).
//!
//! The engine runs in "frame server" mode: it decodes and keeps time (the audio clock when there
//! is sound, the system clock otherwise) and plays the sound itself; this thread asks it for each
//! new frame (`OnVideoStreamTick`) and has it transferred into CPU memory (`TransferVideoFrame`
//! into a WIC bitmap, or into a Direct3D 11 texture that is read back), already turned and
//! scaled. The frame becomes an `egui::ColorImage` here, so the UI thread only uploads it.

use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use instagram_core::poster;
use instagram_core::sniff;
use instagram_core::video_rules::{self, PlaybackFailure, VideoCaps};
use windows::core::{implement, Interface, BSTR, HRESULT, HSTRING};
use windows::Win32::Foundation::{HMODULE, RECT, S_OK};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmap, IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapLockRead, WICRect};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

use super::player::{bgra_to_image, statistics_line, Command, FramePath, Phase, PlayerOptions, PlayerStatus, Shared, VideoFrame, Waker};

/// The multithreaded COM apartment for the life of the player thread. Media Foundation itself is
/// started once per process (`poster::start_media_foundation`) and never shut down.
struct Apartment {
    entered: bool,
}

impl Apartment {
    fn enter() -> Apartment {
        // SAFETY: the player thread is ours; it joins the MTA once and leaves it in drop.
        Apartment { entered: unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok() }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.entered {
            // SAFETY: balances the successful CoInitializeEx above, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// One engine event: (event, param1, param2).
type Event = (u32, usize, u32);

/// The engine's callback: records the event, nothing else. It runs on a Media Foundation
/// work-queue thread WHILE THE ENGINE HOLDS ITS OWN LOCK (seen with cdb in the spike: a version
/// that also locked the shared state here deadlocked against the player thread, which held that
/// state while it called `GetCurrentTime`). So it touches nothing but its own inbox, which is
/// never held while the engine is called, and never calls back into the engine. Nor does it
/// wake the UI: waking takes egui's context lock, which the UI thread holds while it
/// tessellates a frame, and the engine's lock would be held all that time. The player thread
/// looks at the inbox every `PLAYER_TICK_MS` and wakes the UI itself.
#[implement(IMFMediaEngineNotify)]
struct Notify {
    inbox: Arc<Mutex<Vec<Event>>>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows::core::Result<()> {
        if let Ok(mut inbox) = self.inbox.lock() {
            inbox.push((event, param1, param2));
        }
        Ok(())
    }
}

/// Where transferred frames land.
enum Target {
    Wic { factory: IWICImagingFactory, bitmap: Option<(IWICBitmap, u32, u32)> },
    D3d { device: ID3D11Device, context: ID3D11DeviceContext, textures: Option<(ID3D11Texture2D, ID3D11Texture2D, u32, u32)> },
}

fn null_pointer() -> windows::core::Error {
    windows::core::Error::from_hresult(HRESULT(0x8000_4003_u32 as i32))
}

/// "HRESULT 0x........: text", as the core's classification expects it.
fn error_text(code: HRESULT) -> String {
    format!("HRESULT 0x{:08X}: {}", code.0 as u32, windows::core::Error::from_hresult(code).message().trim())
}

/// Direct3D 11 for the engine: the hardware adapter when it can decode video, else WARP.
fn create_device() -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext, &'static str)> {
    let attempts: [(D3D_DRIVER_TYPE, &str); 2] = [(D3D_DRIVER_TYPE_HARDWARE, "hardware"), (D3D_DRIVER_TYPE_WARP, "warp")];
    let mut last = None;
    for (driver, name) in attempts {
        let mut device = None;
        let mut context = None;
        // SAFETY: out-pointers are valid locals; no adapter (the driver type decides).
        let result = unsafe {
            D3D11CreateDevice(None, driver, HMODULE::default(), D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context))
        };
        match (result, device, context) {
            (Ok(()), Some(device), Some(context)) => return Ok((device, context, name)),
            (Err(error), _, _) => last = Some(error),
            _ => {}
        }
    }
    Err(last.unwrap_or_else(null_pointer))
}

/// The engine of one player and what it transfers into.
struct Player {
    engine: IMFMediaEngine,
    engine_ex: IMFMediaEngineEx,
    target: Target,
    caps: VideoCaps,
    target_size: Option<(u32, u32)>,
    serial: u64,
    /// The sound setting the UI asked for (a paused seek mutes for its short play).
    muted: bool,
    /// A seek while paused plays (muted) until its frame arrives, then pauses again.
    pause_after_frame: bool,
}

impl Player {
    /// The engine with its attributes, the target of its frames, and the file as its source.
    fn create(path: &Path, options: &PlayerOptions, mime: &str, notify: IMFMediaEngineNotify) -> windows::core::Result<(Player, String)> {
        // SAFETY (whole function): plain COM / Media Foundation calls with valid arguments on the
        // player thread, which is in the MTA and has Media Foundation started.
        unsafe {
            let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)?;
            let attributes = attributes.ok_or_else(null_pointer)?;
            attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
            attributes.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32)?;
            let wic = |label: &str| -> windows::core::Result<(Target, String)> {
                let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
                Ok((Target::Wic { factory, bitmap: None }, label.to_owned()))
            };
            let (target, adapter) = match options.frame_path {
                FramePath::Wic => wic("wic")?,
                FramePath::D3d11 => match Self::direct3d(&attributes) {
                    Ok(found) => found,
                    Err(error) => {
                        // Measured to work on this machine, unverified on others: fall back.
                        log::warn!("Direct3D 11 for videos is not available ({}); frames go through WIC.", error_text(error.code()));
                        wic("wic (Direct3D 11 unavailable)")?
                    }
                },
            };
            let engine = factory.CreateInstance(0, &attributes)?;
            let engine_ex: IMFMediaEngineEx = engine.cast()?;
            engine.SetMuted(options.muted)?;
            engine.SetVolume(options.volume)?;
            engine.SetLoop(options.looping)?;
            engine.SetAutoPlay(options.autoplay)?;
            engine.SetPreload(MF_MEDIA_ENGINE_PRELOAD_AUTOMATIC)?;

            // The file as a byte stream with the sniffed content type: Media Foundation picks its
            // parser by it, never by the file name (the archive cache names its copies m000001.mp4,
            // a loose file may be misnamed). Measured: the file can still be renamed and deleted
            // by other programs while it plays.
            let stream = MFCreateFile(MF_ACCESSMODE_READ, MF_OPENMODE_FAIL_IF_NOT_EXIST, MF_FILEFLAGS_NONE, &HSTRING::from(path.as_os_str()))?;
            stream.cast::<IMFAttributes>()?.SetString(&MF_BYTESTREAM_CONTENT_TYPE, &HSTRING::from(mime))?;
            let extension = match mime {
                "video/quicktime" => "mov",
                "video/webm" => "webm",
                _ => "mp4",
            };
            engine_ex.SetSourceFromByteStream(&stream, &BSTR::from(format!("video.{extension}")))?;
            let player = Player { engine, engine_ex, target, caps: options.caps, target_size: options.target_size, serial: 0, muted: options.muted, pause_after_frame: false };
            Ok((player, adapter))
        }
    }

    /// A Direct3D 11 device shared with the engine through a DXGI device manager: hardware
    /// decoding where there is a graphics card.
    fn direct3d(attributes: &IMFAttributes) -> windows::core::Result<(Target, String)> {
        let (device, context, name) = create_device()?;
        // SAFETY: COM calls on objects of this thread with valid arguments.
        unsafe {
            // Media Foundation uses the device from its own threads too. The return value is the
            // previous setting.
            let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or_else(null_pointer)?;
            manager.ResetDevice(&device, token)?;
            attributes.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager)?;
        }
        Ok((Target::D3d { device, context, textures: None }, format!("d3d11 {name}")))
    }

    fn apply(&mut self, command: Command) -> windows::core::Result<()> {
        // SAFETY: engine calls on the thread that owns it.
        unsafe {
            match command {
                Command::Play => {
                    self.pause_after_frame = false;
                    self.engine.SetMuted(self.muted)?;
                    self.engine.Play()
                }
                Command::Pause => {
                    self.pause_after_frame = false;
                    self.engine.SetMuted(self.muted)?;
                    self.engine.Pause()
                }
                Command::Seek(seconds, exact) => {
                    let mode = if exact { MF_MEDIA_ENGINE_SEEK_MODE_NORMAL } else { MF_MEDIA_ENGINE_SEEK_MODE_APPROXIMATE };
                    let paused = self.engine.IsPaused().as_bool() || self.pause_after_frame;
                    self.engine_ex.SetCurrentTimeEx(seconds.max(0.0), mode)?;
                    if paused {
                        // Measured: a seek while paused does not always present its frame (3 of
                        // 24 seeks). Playing muted until the first new frame and pausing then
                        // always shows the frame of the new position.
                        self.pause_after_frame = true;
                        self.engine.SetMuted(true)?;
                        self.engine.Play()?;
                    }
                    Ok(())
                }
                Command::Mute(muted) => {
                    self.muted = muted;
                    if self.pause_after_frame {
                        Ok(())
                    } else {
                        self.engine.SetMuted(muted)
                    }
                }
                Command::Volume(volume) => self.engine.SetVolume(volume.clamp(0.0, 1.0)),
                Command::Loop(looping) => self.engine.SetLoop(looping),
                Command::TargetSize(size) => {
                    self.target_size = size;
                    Ok(())
                }
            }
        }
    }

    /// A new frame when the engine has one, or `force` (the first frame, a format change): a
    /// safety net, since `OnVideoStreamTick` may report none for a frame that is shown.
    fn poll_frame(&mut self, force: bool) -> windows::core::Result<Option<VideoFrame>> {
        let mut pts = 0i64;
        // SAFETY: the vtable call is the documented OnVideoStreamTick; it is made directly to
        // tell S_OK (new frame) from S_FALSE (none), which the wrapper maps to the same Ok.
        let hr = unsafe { (Interface::vtable(&self.engine).OnVideoStreamTick)(Interface::as_raw(&self.engine), &mut pts) };
        if hr != S_OK {
            if !force {
                return Ok(None);
            }
            // SAFETY: getter on the owning thread.
            pts = (unsafe { self.engine.GetCurrentTime() } * 10_000_000.0) as i64;
        }
        let (mut width, mut height) = (0u32, 0u32);
        // SAFETY: valid out-pointers.
        unsafe { self.engine.GetNativeVideoSize(Some(&mut width), Some(&mut height))? };
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let (w, h) = video_rules::frame_size_for((width, height), self.target_size, self.caps);
        let rect = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        let border = MFARGB { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbAlpha: 255 };
        let image = match &mut self.target {
            Target::Wic { factory, bitmap } => {
                if bitmap.as_ref().is_none_or(|(_, bw, bh)| (*bw, *bh) != (w, h)) {
                    // SAFETY: a new BGRA bitmap of the wanted size.
                    let created = unsafe { factory.CreateBitmap(w, h, &GUID_WICPixelFormat32bppBGRA, WICBitmapCacheOnLoad)? };
                    *bitmap = Some((created, w, h));
                }
                let Some((bitmap, _, _)) = bitmap.as_ref() else { return Ok(None) };
                // SAFETY: the bitmap is a valid destination surface; rect lies inside it.
                unsafe { self.engine.TransferVideoFrame(bitmap, None, &rect, Some(&border))? };
                let lock_rect = WICRect { X: 0, Y: 0, Width: w as i32, Height: h as i32 };
                // SAFETY: the lock covers the whole bitmap; the data pointer is valid until the
                // lock is released (end of this block), and only `size` bytes are read.
                unsafe {
                    let lock = bitmap.Lock(&lock_rect, WICBitmapLockRead.0 as u32)?;
                    let stride = lock.GetStride()? as usize;
                    let mut size = 0u32;
                    let mut data = std::ptr::null_mut();
                    lock.GetDataPointer(&mut size, &mut data)?;
                    let bytes = std::slice::from_raw_parts(data, size as usize);
                    bgra_to_image(w as usize, h as usize, stride, bytes)
                }
            }
            Target::D3d { device, context, textures } => {
                if textures.as_ref().is_none_or(|(_, _, tw, th)| (*tw, *th) != (w, h)) {
                    let mut desc = D3D11_TEXTURE2D_DESC {
                        Width: w,
                        Height: h,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                        CPUAccessFlags: 0,
                        MiscFlags: 0,
                    };
                    let mut render = None;
                    // SAFETY: valid description and out-pointer.
                    unsafe { device.CreateTexture2D(&desc, None, Some(&mut render))? };
                    desc.Usage = D3D11_USAGE_STAGING;
                    desc.BindFlags = 0;
                    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                    let mut staging = None;
                    // SAFETY: as above.
                    unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging))? };
                    match (render, staging) {
                        (Some(render), Some(staging)) => *textures = Some((render, staging, w, h)),
                        _ => return Ok(None),
                    }
                }
                let Some((render, staging, _, _)) = textures.as_ref() else { return Ok(None) };
                // SAFETY: the render target is on the engine's device; rect lies inside it. The
                // copy and the map are on the same (multithread-protected) device, and only the
                // mapped rows are read.
                unsafe {
                    self.engine.TransferVideoFrame(render, None, &rect, Some(&border))?;
                    context.CopyResource(staging, render);
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                    let bytes = std::slice::from_raw_parts(mapped.pData as *const u8, mapped.RowPitch as usize * h as usize);
                    let image = bgra_to_image(w as usize, h as usize, mapped.RowPitch as usize, bytes);
                    context.Unmap(staging, 0);
                    image
                }
            }
        };
        self.serial += 1;
        if hr == S_OK && self.pause_after_frame {
            // The paused seek of `apply` got its frame: back to paused (and to the sound setting).
            self.pause_after_frame = false;
            // SAFETY: engine calls on the owning thread.
            unsafe {
                let _ = self.engine.Pause();
                let _ = self.engine.SetMuted(self.muted);
            }
        }
        Ok(Some(VideoFrame { image, pts: pts as f64 / 10_000_000.0, serial: self.serial }))
    }

    /// Reads the engine's state into `status`. Called WITHOUT holding the shared lock (see
    /// `Notify`).
    fn update_status(&self, status: &mut PlayerStatus) {
        // SAFETY: plain getters on the owning thread.
        unsafe {
            status.current_time = self.engine.GetCurrentTime();
            status.duration = video_rules::reportable_duration(self.engine.GetDuration());
            status.seeking = self.engine.IsSeeking().as_bool();
            status.has_audio = self.engine.HasAudio().as_bool();
            let (mut w, mut h) = (0u32, 0u32);
            if self.engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)).is_ok() && w > 0 && h > 0 {
                status.native_size = Some((w, h));
            }
            if status.phase != Phase::Failed {
                status.phase = if self.engine.IsEnded().as_bool() {
                    Phase::Ended
                } else if self.engine.GetReadyState() < 1 {
                    Phase::Loading
                } else if self.engine.IsPaused().as_bool() || self.pause_after_frame {
                    Phase::Paused
                } else {
                    Phase::Playing
                };
            }
        }
    }

    /// The engine's error, with the codec when it says "no decoder" or "no source".
    fn failure(&self, path: &Path, mime: &str, param1: usize, param2: u32) -> PlaybackFailure {
        let hresult = HRESULT(param2 as i32);
        let mut media_error = param1 as u16;
        // SAFETY: getters on the owning thread.
        if let Ok(error) = unsafe { self.engine.GetError() } {
            media_error = unsafe { error.GetErrorCode() };
        }
        let codec = if [video_rules::HRESULT_CODEC_NOT_FOUND, video_rules::HRESULT_UNSUPPORTED_BYTESTREAM].contains(&hresult.0) { poster::probe_codec(path, mime) } else { None };
        PlaybackFailure { media_error, hresult: hresult.0, text: error_text(hresult), codec, no_picture: false }
    }

    /// Frames the engine dropped, for the log. Its count of rendered frames is left out: in
    /// frame-server mode it is not the number of frames this thread took. Measured with a 720p
    /// video of 30 frames a second, all of them taken: the engine reported 14 to 299 rendered
    /// frames for the same 20 s, depending on how often the window repainted.
    fn dropped_frames(&self) -> u64 {
        // SAFETY: getter on the owning thread; the PROPVARIANT is read as an integer.
        match unsafe { self.engine_ex.GetStatistics(MF_MEDIA_ENGINE_STATISTIC_FRAMES_DROPPED) } {
            Ok(value) => u64::try_from(&value).ok().or_else(|| u32::try_from(&value).ok().map(u64::from)).unwrap_or(0),
            Err(_) => 0,
        }
    }

    fn shutdown(&self) -> std::time::Duration {
        let started = Instant::now();
        // SAFETY: last call on the engine; it is dropped right after.
        let _ = unsafe { self.engine.Shutdown() };
        started.elapsed()
    }
}

/// The sniffed type of a video file: what the engine's parser is chosen by.
fn sniffed_mime(path: &Path) -> Result<String, String> {
    let head = sniff::read_head(path).map_err(|error| error.to_string())?;
    match sniff::sniff_media(&head) {
        Some(sniffed) if sniffed.kind == instagram_core::model::MediaKind::Video => Ok(sniffed.mime.to_owned()),
        _ => Err("the file is not a video".to_owned()),
    }
}

/// Publishes a failure that happened before the engine existed.
fn fail_early(shared: &Mutex<Shared>, waker: &Waker, mime: String, failure: PlaybackFailure) {
    if let Ok(mut shared) = shared.lock() {
        shared.status.phase = Phase::Failed;
        shared.status.mime = mime;
        shared.status.error = Some(failure);
    }
    waker();
}

/// The player thread; returns how long the engine's shutdown took.
pub(super) fn run(path: &Path, options: PlayerOptions, commands: Receiver<Command>, shared: Arc<Mutex<Shared>>, waker: Waker) -> std::time::Duration {
    let _apartment = Apartment::enter();
    let name = instagram_core::messages::display_name(path);
    let mime = match options.mime.clone().map(Ok).unwrap_or_else(|| sniffed_mime(path)) {
        Ok(mime) => mime,
        Err(reason) => {
            fail_early(&shared, &waker, "video/mp4".to_owned(), PlaybackFailure { media_error: 4, hresult: 0, text: reason, codec: None, no_picture: false });
            return std::time::Duration::ZERO;
        }
    };
    if !poster::start_media_foundation() {
        fail_early(&shared, &waker, mime, PlaybackFailure { media_error: 4, hresult: 0, text: "Media Foundation is not available on this Windows".to_owned(), codec: None, no_picture: false });
        return std::time::Duration::ZERO;
    }
    let inbox: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let notify: IMFMediaEngineNotify = Notify { inbox: Arc::clone(&inbox) }.into();
    // The player thread's own copy of the status; published into `Shared` after every tick.
    let mut status = PlayerStatus { mime: mime.clone(), ..PlayerStatus::default() };
    let (mut player, adapter) = match Player::create(path, &options, &mime, notify) {
        Ok(created) => created,
        Err(error) => {
            let failure = PlaybackFailure { media_error: 0, hresult: error.code().0, text: error_text(error.code()), codec: None, no_picture: false };
            fail_early(&shared, &waker, mime, failure);
            return std::time::Duration::ZERO;
        }
    };
    log::info!("video player for {name} ({adapter})");
    let mut metadata_checked = false;
    // How long it played, for the statistics line.
    let mut playing_for = std::time::Duration::ZERO;
    let mut last_tick = Instant::now();
    loop {
        match commands.recv_timeout(options.tick) {
            Ok(command) => {
                if let Err(error) = player.apply(command) {
                    log::warn!("A video command failed: {}", error_text(error.code()));
                }
                // Every queued command is taken before the next frame.
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let events: Vec<Event> = inbox.lock().map(|mut inbox| std::mem::take(&mut *inbox)).unwrap_or_default();
        let mut force = false;
        let mut changed = false;
        for &(event, param1, param2) in &events {
            if event == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32 && status.phase != Phase::Failed {
                status.phase = Phase::Failed;
                status.error = Some(player.failure(path, &mime, param1, param2));
                changed = true;
            } else if event == MF_MEDIA_ENGINE_EVENT_ENDED.0 as u32 {
                status.ended_events += 1;
                changed = true;
            } else if event == MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY.0 as u32 || event == MF_MEDIA_ENGINE_EVENT_FORMATCHANGE.0 as u32 {
                force = true;
            } else if event == MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA.0 as u32 {
                changed = true;
            }
        }
        if !metadata_checked && status.phase != Phase::Failed {
            // SAFETY: getters on the owning thread.
            let ready = unsafe { player.engine.GetReadyState() } >= 1;
            if ready {
                metadata_checked = true;
                // The clone's rule: metadata without a picture track (width or height 0) is a
                // video that cannot be shown.
                if !unsafe { player.engine.HasVideo() }.as_bool() {
                    status.phase = Phase::Failed;
                    status.has_video = false;
                    status.error = Some(PlaybackFailure { media_error: 4, hresult: 0, text: String::new(), codec: None, no_picture: true });
                    changed = true;
                } else {
                    status.has_video = true;
                }
            }
        }
        let frame = match player.poll_frame(force && status.phase != Phase::Failed) {
            Ok(frame) => frame,
            Err(error) => {
                log::debug!("A video frame could not be transferred: {}", error_text(error.code()));
                None
            }
        };
        let before = (status.phase, status.duration.map(f64::to_bits));
        player.update_status(&mut status);
        changed |= before != (status.phase, status.duration.map(f64::to_bits));
        let now = Instant::now();
        if status.phase == Phase::Playing {
            playing_for += now - last_tick;
        }
        last_tick = now;
        let new_frame = frame.is_some();
        if new_frame {
            status.frames += 1;
        }
        if let Ok(mut state) = shared.lock() {
            state.status = status.clone();
            if frame.is_some() {
                state.frame = frame;
            }
        }
        if new_frame || changed {
            waker();
        }
    }
    log::info!("{}", statistics_line(&name, status.frames, playing_for, player.dropped_frames()));
    let shutdown = player.shutdown();
    drop(player);
    log::info!("video player closed: Shutdown {} ms", shutdown.as_millis());
    shutdown
}
