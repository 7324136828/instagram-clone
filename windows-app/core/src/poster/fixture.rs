//! Test videos made on this machine with Media Foundation's sink writer: H.264 (or HEVC) video
//! with a test pattern and, when asked, AAC sound with a tone, in an MP4 file. Generated
//! fixtures need no licence and no large binary files in the project. Used by the tests of the
//! core and the app; never called by the product.
//!
//! The pattern: a grey gradient, a white bar that moves one step per frame, and the frame number
//! in binary as 16 black or white blocks across the top eighth of the picture, so a decoded frame
//! tells which frame it is (`frame_number`), whatever size it was scaled to.

use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::{HRESULT, HSTRING};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// What to write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixtureSpec {
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// Frames per second.
    pub fps: u32,
    /// Length in seconds.
    pub seconds: f64,
    /// Bits per second of the video.
    pub bitrate: u32,
    /// HEVC instead of H.264.
    pub hevc: bool,
    /// With an AAC tone.
    pub audio: bool,
    /// Degrees clockwise a player must turn the picture (`MF_MT_VIDEO_ROTATION` 0 / 90 / 180 /
    /// 270), as phones write it into the file.
    pub rotation: u32,
}

impl FixtureSpec {
    /// A short H.264 video without sound: 320 x 240, 30 frames per second, 2 seconds.
    pub const SHORT: FixtureSpec = FixtureSpec { width: 320, height: 240, fps: 30, seconds: 2.0, bitrate: 500_000, hevc: false, audio: false, rotation: 0 };
}

/// Number of frame-number blocks across the top of the picture.
pub const BLOCKS: u32 = 16;

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Luma of the pattern at (x, y) for frame `index`.
fn luma(spec: &FixtureSpec, index: u32, x: u32, y: u32) -> u8 {
    let band = spec.height / 8;
    if y < band {
        let block = x * BLOCKS / spec.width;
        let bit = BLOCKS - 1 - block;
        return if index >> bit & 1 == 1 { 235 } else { 16 };
    }
    let bar_width = (spec.width / 40).max(1);
    let bar_x = (index * spec.width / (spec.fps * 2)) % spec.width;
    if x >= bar_x && x < bar_x + bar_width {
        return 235;
    }
    // A diagonal gradient, so that the encoder has something to do.
    (60 + (x + y + index) % 120) as u8
}

/// The frame number in a decoded RGBA picture of the pattern (any size, rows from the top).
pub fn frame_number(width: usize, height: usize, rgba: &[u8]) -> u32 {
    let y = height / 16;
    let mut number = 0;
    for block in 0..BLOCKS as usize {
        let x = (block * 2 + 1) * width / (BLOCKS as usize * 2);
        let at = (y * width + x) * 4;
        let bright = rgba.get(at..at + 3).is_some_and(|pixel| (u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2])) / 3 > 128);
        number = number << 1 | u32::from(bright);
    }
    number
}

fn missing() -> windows::core::Error {
    windows::core::Error::from_hresult(HRESULT(0x8000_4003_u32 as i32))
}

/// Writes the fixture to `path`; returns how long encoding took.
pub fn write_mp4(path: &Path, spec: FixtureSpec) -> windows::core::Result<Duration> {
    let started = Instant::now();
    // SAFETY (whole block): COM for this call on the calling thread (a test thread, never the UI
    // thread), balanced at the end; Media Foundation started once per process; valid arguments
    // throughout; every buffer is written only while it is locked and within its length.
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let result = (|| -> windows::core::Result<()> {
            if !super::start_media_foundation() {
                return Err(missing());
            }
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 2)?;
            let attributes = attributes.ok_or_else(missing)?;
            attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attributes)?;

            let video_out = MFCreateMediaType()?;
            video_out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            video_out.SetGUID(&MF_MT_SUBTYPE, if spec.hevc { &MFVideoFormat_HEVC } else { &MFVideoFormat_H264 })?;
            video_out.SetUINT32(&MF_MT_AVG_BITRATE, spec.bitrate)?;
            video_out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            video_out.SetUINT64(&MF_MT_FRAME_SIZE, pack(spec.width, spec.height))?;
            video_out.SetUINT64(&MF_MT_FRAME_RATE, pack(spec.fps, 1))?;
            video_out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            if spec.rotation != 0 {
                video_out.SetUINT32(&MF_MT_VIDEO_ROTATION, spec.rotation)?;
            }
            let video = writer.AddStream(&video_out)?;

            let video_in = MFCreateMediaType()?;
            video_in.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            video_in.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            video_in.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            video_in.SetUINT64(&MF_MT_FRAME_SIZE, pack(spec.width, spec.height))?;
            video_in.SetUINT64(&MF_MT_FRAME_RATE, pack(spec.fps, 1))?;
            video_in.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            writer.SetInputMediaType(video, &video_in, None)?;

            let audio = if spec.audio {
                let audio_out = MFCreateMediaType()?;
                audio_out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                audio_out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                audio_out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
                audio_out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)?;
                audio_out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                audio_out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 16_000)?;
                let audio = writer.AddStream(&audio_out)?;
                let audio_in = MFCreateMediaType()?;
                audio_in.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                audio_in.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                audio_in.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
                audio_in.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)?;
                audio_in.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                audio_in.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
                audio_in.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 192_000)?;
                writer.SetInputMediaType(audio, &audio_in, None)?;
                Some(audio)
            } else {
                None
            };

            writer.BeginWriting()?;
            let frames = (spec.seconds * f64::from(spec.fps)).round() as u32;
            let frame_100ns = 10_000_000i64 / i64::from(spec.fps);
            let luma_size = (spec.width * spec.height) as usize;
            let nv12_size = luma_size * 3 / 2;
            let audio_frames_per_video = 48_000 / spec.fps;
            let mut audio_written = 0u64;
            for index in 0..frames {
                let buffer = MFCreateMemoryBuffer(nv12_size as u32)?;
                let mut data = std::ptr::null_mut();
                buffer.Lock(&mut data, None, None)?;
                let bytes = std::slice::from_raw_parts_mut(data, nv12_size);
                for y in 0..spec.height {
                    let row = &mut bytes[(y * spec.width) as usize..((y + 1) * spec.width) as usize];
                    for (x, value) in row.iter_mut().enumerate() {
                        *value = luma(&spec, index, x as u32, y);
                    }
                }
                // Neutral chroma: a grey picture.
                bytes[luma_size..].fill(128);
                buffer.Unlock()?;
                buffer.SetCurrentLength(nv12_size as u32)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                sample.SetSampleTime(i64::from(index) * frame_100ns)?;
                sample.SetSampleDuration(frame_100ns)?;
                writer.WriteSample(video, &sample)?;

                if let Some(audio) = audio {
                    let count = audio_frames_per_video as usize;
                    let size = count * 4;
                    let buffer = MFCreateMemoryBuffer(size as u32)?;
                    let mut data = std::ptr::null_mut();
                    buffer.Lock(&mut data, None, None)?;
                    let samples = std::slice::from_raw_parts_mut(data as *mut i16, count * 2);
                    for frame in 0..count {
                        let t = (audio_written + frame as u64) as f64 / 48_000.0;
                        // 440 Hz, and a 1 kHz beep in the first 100 ms of every second.
                        let frequency = if t.fract() < 0.1 { 1000.0 } else { 440.0 };
                        let value = ((t * frequency * std::f64::consts::TAU).sin() * 8000.0) as i16;
                        samples[frame * 2] = value;
                        samples[frame * 2 + 1] = value;
                    }
                    buffer.Unlock()?;
                    buffer.SetCurrentLength(size as u32)?;
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&buffer)?;
                    sample.SetSampleTime((audio_written as i64) * 10_000_000 / 48_000)?;
                    sample.SetSampleDuration(count as i64 * 10_000_000 / 48_000)?;
                    writer.WriteSample(audio, &sample)?;
                    audio_written += count as u64;
                }
            }
            writer.Finalize()?;
            Ok(())
        })();
        if com {
            CoUninitialize();
        }
        result?;
    }
    Ok(started.elapsed())
}

/// A copy of an MP4 file with another major brand in its ftyp box (MOV: "qt  ", M4V: "M4V ").
pub fn rebrand(source: &Path, target: &Path, brand: &[u8; 4]) -> std::io::Result<()> {
    let mut bytes = std::fs::read(source)?;
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        bytes[8..12].copy_from_slice(brand);
    }
    std::fs::write(target, bytes)
}

/// A copy with the first occurrence of the FourCC `from` (a sample entry type) renamed to `to`:
/// a codec Windows lacks. Returns whether it was found.
pub fn replace_fourcc(source: &Path, target: &Path, from: &[u8; 4], to: &[u8; 4]) -> std::io::Result<bool> {
    let mut bytes = std::fs::read(source)?;
    let found = bytes.windows(4).position(|window| window == from);
    if let Some(position) = found {
        bytes[position..position + 4].copy_from_slice(to);
    }
    std::fs::write(target, bytes)?;
    Ok(found.is_some())
}
