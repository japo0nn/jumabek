use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const ECHO_TAIL: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    Idle,
    Listening,
    Speaking,
}

#[derive(Debug)]
struct Inner {
    state: VoiceState,
    muted_until: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct VoiceGate {
    inner: Arc<Mutex<Inner>>,
    tail: Duration,
}

impl VoiceGate {
    pub fn new() -> Self {
        Self::with_tail(ECHO_TAIL)
    }

    pub fn with_tail(tail: Duration) -> Self {
        VoiceGate {
            inner: Arc::new(Mutex::new(Inner {
                state: VoiceState::Idle,
                muted_until: None,
            })),
            tail,
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> VoiceState {
        self.lock().state
    }

    pub fn begin_listening(&self) {
        let mut inner = self.lock();
        if inner.state != VoiceState::Speaking {
            inner.state = VoiceState::Listening;
        }
    }

    pub fn begin_speaking(&self) {
        let mut inner = self.lock();
        inner.state = VoiceState::Speaking;
        inner.muted_until = None;
    }

    pub fn end_speaking(&self) {
        let mut inner = self.lock();
        inner.state = VoiceState::Listening;
        inner.muted_until = Some(Instant::now() + self.tail);
    }

    pub fn idle(&self) {
        let mut inner = self.lock();
        inner.state = VoiceState::Idle;
        inner.muted_until = None;
    }

    pub fn is_capturing(&self) -> bool {
        let inner = self.lock();

        if inner.state != VoiceState::Listening {
            return false;
        }

        match inner.muted_until {
            Some(until) => Instant::now() >= until,
            None => true,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for VoiceGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_idle_and_deaf() {
        let gate = VoiceGate::new();
        assert_eq!(gate.state(), VoiceState::Idle);
        assert!(!gate.is_capturing());
    }

    #[test]
    fn listening_captures() {
        let gate = VoiceGate::new();
        gate.begin_listening();
        assert!(gate.is_capturing());
    }

    #[test]
    fn deaf_while_speaking() {
        let gate = VoiceGate::new();
        gate.begin_listening();
        gate.begin_speaking();
        assert_eq!(gate.state(), VoiceState::Speaking);
        assert!(!gate.is_capturing(), "mic was live while speaking");
    }

    #[test]
    fn stays_deaf_for_the_echo_tail_after_speaking() {
        let gate = VoiceGate::with_tail(Duration::from_millis(120));
        gate.begin_listening();
        gate.begin_speaking();
        gate.end_speaking();

        assert_eq!(gate.state(), VoiceState::Listening);
        assert!(
            !gate.is_capturing(),
            "resumed instantly, echo would leak in"
        );

        std::thread::sleep(Duration::from_millis(160));
        assert!(gate.is_capturing(), "never resumed after the tail");
    }

    #[test]
    fn begin_listening_cannot_interrupt_speech() {
        let gate = VoiceGate::new();
        gate.begin_speaking();
        gate.begin_listening();
        assert_eq!(gate.state(), VoiceState::Speaking);
        assert!(!gate.is_capturing());
    }

    #[test]
    fn shared_across_threads() {
        let gate = VoiceGate::new();
        let mic = gate.clone();

        gate.begin_listening();
        assert!(mic.is_capturing());

        gate.begin_speaking();
        let handle = std::thread::spawn(move || mic.is_capturing());
        assert!(!handle.join().unwrap(), "other thread saw a live mic");
    }
}
