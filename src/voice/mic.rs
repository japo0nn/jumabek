use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::thread;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::error::{JumabekError, JumabekResult};
use crate::voice::state::VoiceGate;
use crate::voice::vad::{FRAME_SAMPLES, SAMPLE_RATE, Vad, VadEvent};

pub struct Mic {
    child: Option<Child>,
}

impl Mic {
    pub fn start(gate: VoiceGate) -> JumabekResult<(Self, UnboundedReceiver<Vec<i16>>)> {
        let mut child = spawn_ffmpeg()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| JumabekError::InternalError("ffmpeg gave no stdout pipe".to_string()))?;

        let (tx, rx) = unbounded_channel();
        thread::spawn(move || capture_loop(stdout, gate, tx));

        Ok((Mic { child: Some(child) }, rx))
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        self.stop();
    }
}

fn capture_loop(mut reader: impl Read, gate: VoiceGate, tx: UnboundedSender<Vec<i16>>) {
    let mut vad = Vad::new();
    let mut raw = vec![0u8; FRAME_SAMPLES * 2];
    let mut was_capturing = false;

    loop {
        if let Err(e) = reader.read_exact(&mut raw) {
            eprintln!("[voice] microphone stream ended: {}", e);
            return;
        }

        let capturing = gate.is_capturing();
        if !capturing {
            if was_capturing {
                vad.reset();
            }
            was_capturing = false;
            continue;
        }
        was_capturing = true;

        let frame: Vec<i16> = raw
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();

        match vad.push_frame(&frame) {
            VadEvent::Utterance(samples) => {
                if tx.send(samples).is_err() {
                    return;
                }
            }
            VadEvent::TooShort | VadEvent::Speaking | VadEvent::Quiet => {}
        }
    }
}

fn spawn_ffmpeg() -> JumabekResult<Child> {
    let (format, input) = capture_source()?;

    Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            &format,
            "-i",
            &input,
            "-ac",
            "1",
            "-ar",
            &SAMPLE_RATE.to_string(),
            "-f",
            "s16le",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| {
            JumabekError::InternalError(format!(
                "cannot start ffmpeg for microphone capture: {} — is ffmpeg installed and on PATH?",
                e
            ))
        })
}

#[cfg(target_os = "windows")]
fn capture_source() -> JumabekResult<(String, String)> {
    let device = first_audio_device()?;
    Ok(("dshow".to_string(), format!("audio={}", device)))
}

#[cfg(target_os = "macos")]
fn capture_source() -> JumabekResult<(String, String)> {
    Ok(("avfoundation".to_string(), ":default".to_string()))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn capture_source() -> JumabekResult<(String, String)> {
    Ok(("pulse".to_string(), "default".to_string()))
}

#[cfg(target_os = "windows")]
fn first_audio_device() -> JumabekResult<String> {
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-list_devices",
            "true",
            "-f",
            "dshow",
            "-i",
            "dummy",
        ])
        .output()
        .map_err(|e| {
            JumabekError::InternalError(format!(
                "cannot ask ffmpeg for audio devices: {} — is ffmpeg installed and on PATH?",
                e
            ))
        })?;

    let listing = String::from_utf8_lossy(&output.stderr);

    parse_audio_devices(&listing)
        .into_iter()
        .next()
        .ok_or_else(|| {
            JumabekError::ConfigError(
                "no microphone found — connect one, or switch [agent].interface back to \"cli\""
                    .to_string(),
            )
        })
}

pub fn parse_audio_devices(listing: &str) -> Vec<String> {
    let mut devices = Vec::new();
    let mut in_audio_section = false;

    for line in listing.lines() {
        let lowered = line.to_lowercase();

        if lowered.contains("audio devices") {
            in_audio_section = true;
            continue;
        }
        if lowered.contains("video devices") {
            in_audio_section = false;
            continue;
        }
        if !in_audio_section || lowered.contains("alternative name") {
            continue;
        }

        if let Some(name) = quoted(line) {
            devices.push(name);
        }
    }

    devices
}

fn quoted(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    let name = &rest[..end];

    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_LISTING: &str = r#"
[dshow @ 000001] "Integrated Camera" (video)
[dshow @ 000001]   Alternative name "@device_pnp_\\?\usb#vid_04f2"
[dshow @ 000001] DirectShow audio devices
[dshow @ 000001]  "Микрофон (Realtek(R) Audio)"
[dshow @ 000001]     Alternative name "@device_cm_{33D9A762}\wave_{9A6B2F44}"
[dshow @ 000001]  "Line In (Realtek(R) Audio)"
[dshow @ 000001]     Alternative name "@device_cm_{33D9A762}\wave_{AB12CD34}"
"#;

    #[test]
    fn picks_device_names_and_skips_alternative_names() {
        let devices = parse_audio_devices(REAL_LISTING);
        assert_eq!(
            devices,
            vec!["Микрофон (Realtek(R) Audio)", "Line In (Realtek(R) Audio)"]
        );
    }

    #[test]
    fn ignores_video_devices() {
        let devices = parse_audio_devices(REAL_LISTING);
        assert!(!devices.iter().any(|d| d.contains("Camera")));
    }

    #[test]
    fn empty_when_nothing_is_connected() {
        let listing = "[dshow @ 0] Could not enumerate video devices (or none found).\n\
                       [dshow @ 0] Could not enumerate audio only devices (or none found).\n\
                       Error opening input file dummy.";
        assert!(parse_audio_devices(listing).is_empty());
    }

    #[test]
    fn survives_garbage() {
        assert!(parse_audio_devices("").is_empty());
        assert!(parse_audio_devices("audio devices\n[x] \"\"").is_empty());
    }
}
