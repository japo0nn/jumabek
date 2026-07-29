use colored::Colorize;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::core::task::Choice;
use crate::error::{JumabekError, JumabekResult};
use crate::interfaces::UserInterface;

const PROMPT: &str = "\x1b[38;5;39m❯\x1b[0m ";

pub struct Cli {
    editor: DefaultEditor,
}

impl Cli {
    pub fn new() -> JumabekResult<Self> {
        Ok(Cli {
            editor: DefaultEditor::new()?,
        })
    }

    fn print_banner(&self) {
        let name = "JumaBek".bright_cyan().bold();
        let tagline = "your machine, spoken to".dimmed();
        println!();
        println!("  {} {}", name, tagline);
        println!(
            "  {}",
            "type a task, or ctrl-c / ctrl-d to leave".bright_black()
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
        println!("  {} {}", "→".bright_cyan(), choice.label.bright_white());
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
                    println!("{}", "  bye".bright_black());
                    return Ok(None);
                }
                Err(e) => return Err(JumabekError::from(e)),
            }
        }
    }

    async fn show_response(&mut self, text: &str) -> JumabekResult<()> {
        println!();
        for line in text.lines() {
            println!("  {}", line);
        }
        println!();
        Ok(())
    }

    async fn show_status(&mut self, text: &str) -> JumabekResult<()> {
        println!("  {} {}", "·".bright_black(), text.bright_black());
        Ok(())
    }

    async fn show_error(&mut self, text: &str) -> JumabekResult<()> {
        println!();
        println!("  {} {}", "✗".red().bold(), text.red());
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
            "  {} {}  {}",
            "permission".yellow().bold(),
            Self::risk_badge(risk_level),
            action.bright_white().bold()
        );
        println!("  {}", description.white());
        println!();

        loop {
            let answer = self.ask_line(&format!(
                "  {} {} ",
                "allow?".yellow(),
                "[y/N]".bright_black()
            ))?;

            let Some(answer) = answer else {
                println!("  {}", "denied".red());
                return Ok(false);
            };

            match answer.to_lowercase().as_str() {
                "y" | "yes" | "д" | "да" => {
                    println!("  {}", "allowed".green());
                    println!();
                    return Ok(true);
                }
                "" | "n" | "no" | "н" | "нет" => {
                    println!("  {}", "denied".red());
                    println!();
                    return Ok(false);
                }
                _ => {
                    println!("  {}", "answer y or n".bright_black());
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
        println!("  {} {}", "?".bright_cyan().bold(), message.bright_white());
        println!();
        for (i, option) in options.iter().enumerate() {
            let index = format!("{})", i + 1).bright_cyan();
            if option.label == option.value {
                println!("    {} {}", index, option.value);
            } else {
                println!(
                    "    {} {}  {}",
                    index,
                    option.label.bright_white(),
                    option.value.bright_black()
                );
            }
        }
        println!();

        loop {
            let answer = self.ask_line(&format!(
                "  {} {} ",
                "pick".bright_cyan(),
                format!("[1-{}]", options.len()).bright_black()
            ))?;

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
                "  {}",
                format!("enter a number from 1 to {}", options.len()).bright_black()
            );
        }
    }
}
