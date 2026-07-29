pub const SAMPLE_RATE: u32 = 16_000;
pub const FRAME_SAMPLES: usize = 480;

const FRAME_MS: usize = FRAME_SAMPLES * 1000 / SAMPLE_RATE as usize;
const SILENCE_TIMEOUT_MS: usize = 900;
const SILENCE_FRAMES: usize = SILENCE_TIMEOUT_MS / FRAME_MS;
const MIN_SPEECH_SAMPLES: usize = SAMPLE_RATE as usize / 2;
const MAX_SPEECH_SAMPLES: usize = SAMPLE_RATE as usize * 30;

const MIN_RMS_FOR_VOICE: f64 = 50.0;
const START_FACTOR: f64 = 3.5;
const KEEP_FACTOR: f64 = 2.0;
const NOISE_FLOOR_MIN: f64 = 10.0;

#[derive(Debug, PartialEq)]
pub enum VadEvent {
    Quiet,
    Speaking,
    Utterance(Vec<i16>),
    TooShort,
}

const TRAILING_SILENCE_KEPT: usize = 2;

pub struct Vad {
    noise_floor: f64,
    speaking: bool,
    silent_frames: usize,
    voiced_samples: usize,
    buffer: Vec<i16>,
    hpf: HighPass,
}

impl Vad {
    pub fn new() -> Self {
        Vad {
            noise_floor: 80.0,
            speaking: false,
            silent_frames: 0,
            voiced_samples: 0,
            buffer: Vec::new(),
            hpf: HighPass::new(),
        }
    }

    pub fn reset(&mut self) {
        self.speaking = false;
        self.silent_frames = 0;
        self.voiced_samples = 0;
        self.buffer.clear();
    }

    #[cfg(test)]
    pub fn noise_floor(&self) -> f64 {
        self.noise_floor
    }

    pub fn push_frame(&mut self, frame: &[i16]) -> VadEvent {
        let mut frame = frame.to_vec();
        self.hpf.process(&mut frame);

        let rms = rms(&frame);
        let factor = if self.speaking {
            KEEP_FACTOR
        } else {
            START_FACTOR
        };
        let is_voice = rms > self.noise_floor * factor && rms > MIN_RMS_FOR_VOICE;

        if !is_voice {
            self.noise_floor = (self.noise_floor * 0.95 + rms * 0.05).max(NOISE_FLOOR_MIN);
        }

        if is_voice {
            if !self.speaking {
                self.speaking = true;
                self.buffer.clear();
                self.voiced_samples = 0;
            }
            self.buffer.extend_from_slice(&frame);
            self.voiced_samples += frame.len();
            self.silent_frames = 0;

            if self.buffer.len() >= MAX_SPEECH_SAMPLES {
                return self.finish();
            }

            return VadEvent::Speaking;
        }

        if !self.speaking {
            return VadEvent::Quiet;
        }

        self.buffer.extend_from_slice(&frame);
        self.silent_frames += 1;

        if self.silent_frames >= SILENCE_FRAMES {
            return self.finish();
        }

        VadEvent::Speaking
    }

    fn finish(&mut self) -> VadEvent {
        let trailing = self.silent_frames.saturating_sub(TRAILING_SILENCE_KEPT) * FRAME_SAMPLES;
        let voiced = self.voiced_samples;

        self.speaking = false;
        self.silent_frames = 0;
        self.voiced_samples = 0;

        let mut captured = std::mem::take(&mut self.buffer);

        if voiced < MIN_SPEECH_SAMPLES {
            return VadEvent::TooShort;
        }

        let keep = captured.len().saturating_sub(trailing);
        captured.truncate(keep);

        normalize(&mut captured);
        VadEvent::Utterance(captured)
    }
}

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}

fn rms(frame: &[i16]) -> f64 {
    if frame.is_empty() {
        return 0.0;
    }
    (frame.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / frame.len() as f64).sqrt()
}

fn normalize(samples: &mut [i16]) {
    let peak = samples.iter().map(|&s| s.unsigned_abs()).max().unwrap_or(0);
    if peak == 0 {
        return;
    }

    let gain = (0.95 * 32768.0 / peak as f64).min(10.0);
    for sample in samples.iter_mut() {
        *sample = ((*sample as f64) * gain).round().clamp(-32768.0, 32767.0) as i16;
    }
}

struct HighPass {
    prev_x: f64,
    prev_y: f64,
}

impl HighPass {
    fn new() -> Self {
        HighPass {
            prev_x: 0.0,
            prev_y: 0.0,
        }
    }

    fn process(&mut self, frame: &mut [i16]) {
        for sample in frame.iter_mut() {
            let x = *sample as f64;
            let y = x - self.prev_x + 0.999 * self.prev_y;
            self.prev_x = x;
            self.prev_y = y;
            *sample = y.round().clamp(-32768.0, 32767.0) as i16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silence() -> Vec<i16> {
        vec![0; FRAME_SAMPLES]
    }

    fn tone(amplitude: f64) -> Vec<i16> {
        (0..FRAME_SAMPLES)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE as f64;
                (amplitude * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
            })
            .collect()
    }

    fn feed(vad: &mut Vad, frame: &[i16], times: usize) -> Vec<VadEvent> {
        (0..times).map(|_| vad.push_frame(frame)).collect()
    }

    #[test]
    fn silence_stays_quiet() {
        let mut vad = Vad::new();
        for event in feed(&mut vad, &silence(), 40) {
            assert_eq!(event, VadEvent::Quiet);
        }
    }

    #[test]
    fn speech_then_silence_yields_an_utterance() {
        let mut vad = Vad::new();
        feed(&mut vad, &silence(), 20);

        let during = feed(&mut vad, &tone(6000.0), 40);
        assert!(during.iter().all(|e| *e == VadEvent::Speaking));

        let mut got = None;
        for _ in 0..SILENCE_FRAMES + 5 {
            if let VadEvent::Utterance(samples) = vad.push_frame(&silence()) {
                got = Some(samples);
                break;
            }
        }

        let samples = got.expect("utterance was never emitted");
        assert!(samples.len() >= MIN_SPEECH_SAMPLES);
    }

    #[test]
    fn trailing_silence_is_trimmed_off_the_utterance() {
        let mut vad = Vad::new();
        feed(&mut vad, &silence(), 20);

        let voiced_frames = 40;
        feed(&mut vad, &tone(6000.0), voiced_frames);

        let mut got = None;
        for _ in 0..SILENCE_FRAMES + 5 {
            if let VadEvent::Utterance(samples) = vad.push_frame(&silence()) {
                got = Some(samples);
                break;
            }
        }

        let samples = got.expect("utterance was never emitted");
        let voiced = voiced_frames * FRAME_SAMPLES;
        let cap = voiced + (TRAILING_SILENCE_KEPT + 1) * FRAME_SAMPLES;

        assert!(
            samples.len() <= cap,
            "kept {} samples, expected at most {} — trailing silence was not trimmed",
            samples.len(),
            cap
        );
    }

    #[test]
    fn a_short_blip_is_discarded() {
        let mut vad = Vad::new();
        feed(&mut vad, &silence(), 20);
        feed(&mut vad, &tone(6000.0), 2);

        let mut verdict = None;
        for _ in 0..SILENCE_FRAMES + 5 {
            match vad.push_frame(&silence()) {
                VadEvent::Speaking => {}
                other => {
                    verdict = Some(other);
                    break;
                }
            }
        }

        assert_eq!(verdict, Some(VadEvent::TooShort));
    }

    #[test]
    fn quiet_room_noise_does_not_trigger_speech() {
        let mut vad = Vad::new();
        for event in feed(&mut vad, &tone(20.0), 60) {
            assert_eq!(event, VadEvent::Quiet, "noise floor {}", vad.noise_floor());
        }
    }

    #[test]
    fn endless_speech_is_cut_at_the_cap() {
        let mut vad = Vad::new();
        feed(&mut vad, &silence(), 20);

        let frames_needed = MAX_SPEECH_SAMPLES / FRAME_SAMPLES + 2;
        let mut cut = false;
        for _ in 0..frames_needed {
            if let VadEvent::Utterance(samples) = vad.push_frame(&tone(6000.0)) {
                assert!(samples.len() >= MAX_SPEECH_SAMPLES);
                cut = true;
                break;
            }
        }

        assert!(cut, "speech was never cut off");
    }

    #[test]
    fn reset_drops_a_half_captured_utterance() {
        let mut vad = Vad::new();
        feed(&mut vad, &silence(), 20);
        feed(&mut vad, &tone(6000.0), 30);

        vad.reset();

        for _ in 0..SILENCE_FRAMES + 5 {
            assert_eq!(vad.push_frame(&silence()), VadEvent::Quiet);
        }
    }

    #[test]
    fn normalize_lifts_a_quiet_signal() {
        let mut samples = tone(1000.0);
        normalize(&mut samples);
        let peak = samples.iter().map(|&s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 8_000, "peak stayed at {peak}");
    }
}
