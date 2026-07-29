pub mod mic;
pub mod speech;
pub mod state;
pub mod stt;
pub mod tts;
pub mod vad;
pub mod wav;

use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::core::task::Choice;
use crate::error::{JumabekError, JumabekResult};
use crate::interfaces::UserInterface;
use crate::voice::mic::Mic;
use crate::voice::state::VoiceGate;
use crate::voice::stt::Stt;
use crate::voice::tts::Tts;

pub struct Voice {
    gate: VoiceGate,
    tts: Tts,
    stt: Stt,
    #[allow(dead_code)]
    mic: Mic,
    utterances: Mutex<UnboundedReceiver<Vec<i16>>>,
    echo_to_terminal: bool,
}

impl Voice {
    pub fn start(
        groq_api_key: impl Into<String>,
        voice_name: Option<String>,
        language: Option<String>,
        echo_to_terminal: bool,
    ) -> JumabekResult<Self> {
        let gate = VoiceGate::new();
        let (mic, utterances) = Mic::start(gate.clone())?;

        Ok(Voice {
            tts: Tts::new(gate.clone(), voice_name),
            stt: Stt::new(groq_api_key, language)?,
            mic,
            utterances: Mutex::new(utterances),
            gate,
            echo_to_terminal,
        })
    }

    async fn say(&self, text: &str) -> JumabekResult<()> {
        let spoken = speech::to_speakable(text);
        if spoken.is_empty() {
            return Ok(());
        }

        if self.echo_to_terminal {
            println!("  {}", spoken);
        }

        self.tts.speak(&spoken).await
    }

    async fn listen(&mut self) -> JumabekResult<Option<String>> {
        self.gate.begin_listening();

        if self.echo_to_terminal {
            println!("  · listening");
        }

        let mut utterances = self.utterances.lock().await;

        loop {
            let Some(samples) = utterances.recv().await else {
                return Err(JumabekError::InternalError(
                    "the microphone capture thread stopped".to_string(),
                ));
            };

            if self.echo_to_terminal {
                println!(
                    "  · heard {:.1}s, transcribing",
                    samples.len() as f64 / vad::SAMPLE_RATE as f64
                );
            }

            let text = self.stt.transcribe(&samples).await?;
            if text.trim().is_empty() {
                if self.echo_to_terminal {
                    println!("  · nothing recognisable in that, still listening");
                }
                continue;
            }
            if self.echo_to_terminal {
                println!("  you  {}", text);
            }
            return Ok(Some(text));
        }
    }

    fn first_word(answer: &str) -> Option<String> {
        let cleaned: String = answer
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect();

        cleaned
            .split_whitespace()
            .next()
            .map(|word| word.to_string())
    }

    fn is_affirmative(answer: &str) -> bool {
        Self::first_word(answer).is_some_and(|word| {
            matches!(
                word.as_str(),
                "да" | "ага"
                    | "давай"
                    | "разрешаю"
                    | "конечно"
                    | "yes"
                    | "yeah"
                    | "ok"
                    | "okay"
            )
        })
    }

    fn is_negative(answer: &str) -> bool {
        Self::first_word(answer).is_some_and(|word| {
            matches!(
                word.as_str(),
                "нет" | "не" | "отмена" | "отклоняю" | "стоп" | "no" | "nope" | "cancel"
            )
        })
    }

    fn match_choice(answer: &str, options: &[Choice]) -> Option<String> {
        let cleaned = answer.to_lowercase();

        for (index, option) in options.iter().enumerate() {
            let ordinal = index + 1;
            if cleaned.contains(&ordinal.to_string()) || cleaned.contains(spoken_ordinal(ordinal)) {
                return Some(option.value.clone());
            }
        }

        options
            .iter()
            .find(|option| cleaned.contains(&option.label.to_lowercase()))
            .map(|option| option.value.clone())
    }
}

fn spoken_ordinal(index: usize) -> &'static str {
    match index {
        1 => "перв",
        2 => "втор",
        3 => "трет",
        4 => "четверт",
        5 => "пят",
        _ => "\u{0}",
    }
}

#[async_trait::async_trait]
impl UserInterface for Voice {
    async fn banner(&mut self) -> JumabekResult<()> {
        if self.echo_to_terminal {
            println!();
            println!("  voice mode — speak, or say выход to leave");
            println!("  if it never hears you, run: jumabek mic");
            println!();
        }
        Ok(())
    }

    async fn read_request(&mut self) -> JumabekResult<Option<String>> {
        let text = self.listen().await?;

        if let Some(text) = &text {
            let lowered = text.to_lowercase();
            let trimmed = lowered.trim_end_matches(['.', '!', '?']).trim();
            if ["выход", "выйди", "стоп", "закройся", "exit", "quit"].contains(&trimmed)
            {
                self.gate.idle();
                return Ok(None);
            }
        }

        Ok(text)
    }

    async fn show_response(&mut self, text: &str) -> JumabekResult<()> {
        self.say(&crate::interfaces::markdown::to_speech(text))
            .await
    }

    async fn show_status(&mut self, text: &str) -> JumabekResult<()> {
        if self.echo_to_terminal {
            println!("  · {}", text);
        }
        Ok(())
    }

    async fn show_error(&mut self, text: &str) -> JumabekResult<()> {
        self.say(text).await
    }

    async fn ask_permission(
        &mut self,
        action: &str,
        description: &str,
        risk_level: &str,
    ) -> JumabekResult<bool> {
        self.say(&format!(
            "Нужно разрешение, уровень риска {}. {}. {}. Разрешить?",
            risk_level, action, description
        ))
        .await?;

        for _ in 0..3 {
            let Some(answer) = self.listen().await? else {
                return Ok(false);
            };

            if Self::is_affirmative(&answer) {
                return Ok(true);
            }
            if Self::is_negative(&answer) {
                return Ok(false);
            }

            self.say("Не понял. Скажи да или нет.").await?;
        }

        self.say("Не расслышал ответа, считаю это отказом.").await?;
        Ok(false)
    }

    async fn prompt_choice(&mut self, message: &str, options: &[Choice]) -> JumabekResult<String> {
        if options.is_empty() {
            return Err(JumabekError::InternalError(
                "prompt_choice called with no options".to_string(),
            ));
        }

        let listed = options
            .iter()
            .enumerate()
            .map(|(i, o)| format!("{}. {}", i + 1, o.label))
            .collect::<Vec<_>>()
            .join(". ");

        self.say(&format!("{}. Варианты: {}", message, listed))
            .await?;

        for _ in 0..3 {
            let Some(answer) = self.listen().await? else {
                return Ok(options[0].value.clone());
            };

            if let Some(value) = Self::match_choice(&answer, options) {
                return Ok(value);
            }

            self.say("Не понял. Назови номер варианта.").await?;
        }

        Ok(options[0].value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Vec<Choice> {
        vec![
            Choice::new("в Документах", "C:/Users/sosa/Documents/doc.txt"),
            Choice::new("в Загрузках", "C:/Users/sosa/Downloads/doc.txt"),
        ]
    }

    #[test]
    fn hears_yes_and_no() {
        for yes in ["да", "Да.", "ага", "давай", "разрешаю", "yes", "ok"] {
            assert!(Voice::is_affirmative(yes), "missed yes: {yes}");
            assert!(!Voice::is_negative(yes), "yes read as no: {yes}");
        }

        for no in ["нет", "Нет!", "отмена", "стоп", "no", "cancel"] {
            assert!(Voice::is_negative(no), "missed no: {no}");
            assert!(!Voice::is_affirmative(no), "no read as yes: {no}");
        }
    }

    #[test]
    fn unclear_answers_are_neither() {
        for unclear in ["может быть", "погоди", "что", ""] {
            assert!(!Voice::is_affirmative(unclear), "{unclear}");
            assert!(!Voice::is_negative(unclear), "{unclear}");
        }
    }

    #[test]
    fn a_yes_later_in_the_sentence_is_not_consent() {
        assert!(!Voice::is_affirmative("нет, не надо, да ну его"));
        assert!(Voice::is_negative("нет, не надо, да ну его"));
    }

    #[test]
    fn picks_a_choice_by_ordinal() {
        assert_eq!(
            Voice::match_choice("второй", &options()).unwrap(),
            "C:/Users/sosa/Downloads/doc.txt"
        );
        assert_eq!(
            Voice::match_choice("давай 1", &options()).unwrap(),
            "C:/Users/sosa/Documents/doc.txt"
        );
    }

    #[test]
    fn picks_a_choice_by_label() {
        assert_eq!(
            Voice::match_choice("тот что в загрузках", &options()).unwrap(),
            "C:/Users/sosa/Downloads/doc.txt"
        );
    }

    #[test]
    fn returns_nothing_when_the_answer_matches_no_option() {
        assert!(Voice::match_choice("не знаю", &options()).is_none());
    }
}
