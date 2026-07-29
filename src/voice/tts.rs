use tokio::process::Command;

use crate::error::{JumabekError, JumabekResult};
use crate::voice::state::VoiceGate;

pub struct Tts {
    gate: VoiceGate,
    voice_name: Option<String>,
}

impl Tts {
    pub fn new(gate: VoiceGate, voice_name: Option<String>) -> Self {
        Tts { gate, voice_name }
    }

    pub async fn speak(&self, text: &str) -> JumabekResult<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }

        self.gate.begin_speaking();
        let result = self.run(text).await;
        self.gate.end_speaking();

        result
    }

    async fn run(&self, text: &str) -> JumabekResult<()> {
        let mut command = self.build_command(text)?;

        let status = command.status().await.map_err(|e| {
            JumabekError::InternalError(format!("cannot start the speech synthesiser: {}", e))
        })?;

        if !status.success() {
            return Err(JumabekError::InternalError(format!(
                "speech synthesiser exited with {}",
                status
            )));
        }

        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn build_command(&self, text: &str) -> JumabekResult<Command> {
        let select_voice = match &self.voice_name {
            Some(name) => format!("$s.SelectVoice('{}');", escape_ps(name)),
            None => String::new(),
        };

        let script = format!(
            "Add-Type -AssemblyName System.Speech; \
             $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
             {} $s.Speak('{}');",
            select_voice,
            escape_ps(text)
        );

        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        Ok(command)
    }

    #[cfg(target_os = "macos")]
    fn build_command(&self, text: &str) -> JumabekResult<Command> {
        let mut command = Command::new("say");
        if let Some(name) = &self.voice_name {
            command.args(["-v", name]);
        }
        command.arg(text);
        Ok(command)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn build_command(&self, text: &str) -> JumabekResult<Command> {
        let mut command = Command::new("spd-say");
        command.arg("--wait");
        if let Some(name) = &self.voice_name {
            command.args(["-y", name]);
        }
        command.arg(text);
        Ok(command)
    }
}

#[cfg(target_os = "windows")]
fn escape_ps(text: &str) -> String {
    text.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::state::VoiceState;

    #[cfg(target_os = "windows")]
    #[test]
    fn every_quote_is_doubled() {
        let raw = "it's fine'; Remove-Item; '";
        let escaped = escape_ps(raw);
        assert_eq!(
            escaped.matches('\'').count(),
            raw.matches('\'').count() * 2,
            "escaping left an odd quote: {escaped}"
        );
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn injected_powershell_is_treated_as_plain_text() {
        let payload = "hello'; Write-Output 'PWNED";
        let script = format!("Write-Output '{}'", escape_ps(payload));

        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .await
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().map(|l| l.trim()).collect();

        assert_eq!(
            lines,
            vec![payload],
            "the payload executed instead of being spoken verbatim"
        );
    }

    #[tokio::test]
    async fn empty_text_does_not_touch_the_gate() {
        let gate = VoiceGate::new();
        gate.begin_listening();

        Tts::new(gate.clone(), None).speak("   ").await.unwrap();

        assert_eq!(gate.state(), VoiceState::Listening);
        assert!(gate.is_capturing());
    }
}
