use std::borrow::Cow;

use colored::Colorize;
use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Cmd, Editor, EventHandler, Helper, KeyCode, KeyEvent, Modifiers};

use crate::core::task::Choice;
use crate::error::{JumabekError, JumabekResult};
use crate::interfaces::UserInterface;
use crate::interfaces::markdown;

const PROMPT: &str = "  you  ";
const INDENT: &str = "  ";
const FALLBACK_WIDTH: usize = 80;

const CHIP_BG: (u8, u8, u8) = (31, 42, 55);
const BAR: (u8, u8, u8) = (66, 132, 152);

/// Colour reaches the prompt through here rather than through the prompt
/// string. rustyline measures a prompt by counting its characters, so an escape
/// sequence embedded in it is counted as width the cursor does not occupy —
/// which pushes input to the right and breaks the line wrapping that follows.
struct Prompt;

impl Highlighter for Prompt {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        if prompt == PROMPT {
            let (r, g, b) = CHIP_BG;
            return Cow::Owned(
                prompt
                    .bright_white()
                    .bold()
                    .on_truecolor(r, g, b)
                    .to_string(),
            );
        }
        Cow::Owned(prompt.bright_cyan().bold().to_string())
    }
}

impl Completer for Prompt {
    type Candidate = String;
}
impl Hinter for Prompt {
    type Hint = String;
}
impl Validator for Prompt {}
impl Helper for Prompt {}

pub struct Cli {
    editor: Editor<Prompt, DefaultHistory>,
}

fn terminal_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w as usize)
        .unwrap_or(FALLBACK_WIDTH)
}

fn body_width() -> usize {
    terminal_width().saturating_sub(INDENT.len() * 2)
}

impl Cli {
    pub fn new() -> JumabekResult<Self> {
        let mut editor: Editor<Prompt, DefaultHistory> = Editor::new()?;
        editor.set_helper(Some(Prompt));

        // Shift+Enter is only distinguishable on terminals that report
        // modifiers, so Alt+Enter carries the same binding for the rest.
        for key in [
            KeyEvent(KeyCode::Enter, Modifiers::SHIFT),
            KeyEvent(KeyCode::Enter, Modifiers::ALT),
        ] {
            editor.bind_sequence(key, EventHandler::Simple(Cmd::Insert(1, "\n".to_string())));
        }

        Ok(Cli { editor })
    }

    fn print_banner(&self) {
        println!();
        println!(
            "{}{} {}",
            INDENT,
            "JumaBek".bright_cyan().bold(),
            "your machine, spoken to".bright_black()
        );
        println!(
            "{}{}",
            INDENT,
            "type a task · shift+enter for a new line · ctrl-c to leave".bright_black()
        );
        println!();
    }

    fn risk_badge(risk_level: &str) -> String {
        match risk_level.to_lowercase().as_str() {
            "low" => " LOW ".black().on_green().to_string(),
            "medium" => " MEDIUM ".black().on_yellow().to_string(),
            "high" => " HIGH ".white().on_red().bold().to_string(),
            other => format!(" {} ", other.to_uppercase())
                .black()
                .on_white()
                .to_string(),
        }
    }

    fn confirm_pick(choice: &Choice) -> String {
        println!(
            "{}{} {}",
            INDENT,
            "→".bright_cyan(),
            choice.label.bright_white()
        );
        println!();
        choice.value.clone()
    }

    fn ask_line(&mut self, prompt: &str) -> JumabekResult<Option<String>> {
        match self.editor.readline(prompt) {
            Ok(line) => Ok(Some(line.trim().to_string())),
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => Ok(None),
            Err(e) => Err(JumabekError::from(e)),
        }
    }
}

#[async_trait::async_trait]
impl UserInterface for Cli {
    async fn banner(&mut self) -> JumabekResult<()> {
        self.print_banner();
        Ok(())
    }

    async fn read_request(&mut self) -> JumabekResult<Option<String>> {
        loop {
            match self.editor.readline(PROMPT) {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if matches!(trimmed, "exit" | "quit" | ":q") {
                        return Ok(None);
                    }
                    let _ = self.editor.add_history_entry(trimmed);
                    return Ok(Some(trimmed.to_string()));
                }
                Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                    println!("{}{}", INDENT, "bye".bright_black());
                    return Ok(None);
                }
                Err(e) => return Err(JumabekError::from(e)),
            }
        }
    }

    async fn show_response(&mut self, text: &str) -> JumabekResult<()> {
        let (r, g, b) = BAR;
        let bar = "▌".truecolor(r, g, b).to_string();

        println!();
        for line in markdown::render(text, body_width().saturating_sub(2)) {
            if line.is_empty() {
                println!("{}{}", INDENT, bar);
            } else {
                println!("{}{} {}", INDENT, bar, line);
            }
        }
        println!();
        Ok(())
    }

    async fn show_status(&mut self, text: &str) -> JumabekResult<()> {
        println!("{}{} {}", INDENT, "·".bright_black(), text.bright_black());
        Ok(())
    }

    async fn show_error(&mut self, text: &str) -> JumabekResult<()> {
        let bar = "▌".red().to_string();

        println!();
        for line in markdown::render(text, body_width().saturating_sub(2)) {
            println!("{}{} {}", INDENT, bar, line.red());
        }
        println!();
        Ok(())
    }

    async fn ask_permission(
        &mut self,
        action: &str,
        description: &str,
        risk_level: &str,
    ) -> JumabekResult<bool> {
        println!();
        println!(
            "{}{} {}  {}",
            INDENT,
            "permission".yellow().bold(),
            Self::risk_badge(risk_level),
            action.bright_white().bold()
        );
        for line in markdown::render(description, body_width()) {
            println!("{}{}", INDENT, line);
        }
        println!();

        loop {
            let answer = self.ask_line(&format!("{}allow? [y/N] ", INDENT))?;

            let Some(answer) = answer else {
                println!("{}{}", INDENT, "denied".red());
                return Ok(false);
            };

            match answer.to_lowercase().as_str() {
                "y" | "yes" | "д" | "да" => {
                    println!("{}{}", INDENT, "allowed".green());
                    println!();
                    return Ok(true);
                }
                "" | "n" | "no" | "н" | "нет" => {
                    println!("{}{}", INDENT, "denied".red());
                    println!();
                    return Ok(false);
                }
                _ => {
                    println!("{}{}", INDENT, "answer y or n".bright_black());
                }
            }
        }
    }

    async fn prompt_choice(&mut self, message: &str, options: &[Choice]) -> JumabekResult<String> {
        if options.is_empty() {
            return Err(JumabekError::InternalError(
                "prompt_choice called with no options".to_string(),
            ));
        }

        println!();
        for (n, line) in markdown::render(message, body_width())
            .into_iter()
            .enumerate()
        {
            if n == 0 {
                println!("{}{} {}", INDENT, "?".bright_cyan().bold(), line);
            } else {
                println!("{}  {}", INDENT, line);
            }
        }
        println!();
        for (i, option) in options.iter().enumerate() {
            let index = format!("{})", i + 1).bright_cyan();
            if option.label == option.value {
                println!("{}  {} {}", INDENT, index, option.value);
            } else {
                println!(
                    "{}  {} {}  {}",
                    INDENT,
                    index,
                    option.label.bright_white(),
                    option.value.bright_black()
                );
            }
        }
        println!();

        loop {
            let answer = self.ask_line(&format!("{}pick [1-{}] ", INDENT, options.len()))?;

            let Some(answer) = answer else {
                return Ok(options[0].value.clone());
            };

            if let Ok(n) = answer.parse::<usize>()
                && n >= 1
                && n <= options.len()
            {
                return Ok(Self::confirm_pick(&options[n - 1]));
            }

            if let Some(matched) = options.iter().find(|o| {
                o.label.eq_ignore_ascii_case(answer.as_str())
                    || o.value.eq_ignore_ascii_case(answer.as_str())
            }) {
                return Ok(Self::confirm_pick(matched));
            }

            println!(
                "{}{}",
                INDENT,
                format!("enter a number from 1 to {}", options.len()).bright_black()
            );
        }
    }
}
