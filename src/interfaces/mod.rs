pub mod cli;

use crate::core::task::Choice;
use crate::error::JumabekResult;

#[async_trait::async_trait]
pub trait UserInterface: Send + Sync {
    async fn banner(&mut self) -> JumabekResult<()> {
        Ok(())
    }

    async fn read_request(&mut self) -> JumabekResult<Option<String>>;

    async fn show_response(&mut self, text: &str) -> JumabekResult<()>;

    async fn show_status(&mut self, text: &str) -> JumabekResult<()>;

    async fn show_error(&mut self, text: &str) -> JumabekResult<()>;

    async fn ask_permission(
        &mut self,
        action: &str,
        description: &str,
        risk_level: &str,
    ) -> JumabekResult<bool>;

    async fn prompt_choice(&mut self, message: &str, options: &[Choice]) -> JumabekResult<String>;
}
