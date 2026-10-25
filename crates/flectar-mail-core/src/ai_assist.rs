//! Preset-driven text assistance: summarize, shorten, rewrite or draft a piece
//! of text with a user-written instruction. Uses the configured AI endpoint
//! and the drafting model tier.

use crate::ai::{self, ChatMessage};
use crate::error::Result;
use crate::{Core, Scenario};

/// Longest input sent to the model, in characters.
const MAX_ASSIST_INPUT_CHARS: usize = 60_000;

/// The chat messages for one assist request. An empty `text` asks the model
/// to write new content from the instruction alone.
pub fn assist_prompt(instruction: &str, text: &str) -> Vec<ChatMessage> {
    let system = "You are a writing assistant inside an email client. Follow the \
                  user's instruction exactly. Reply with the resulting text only: no \
                  preamble, no explanations, no quotes around it, no markdown code \
                  fences. Unless the instruction says otherwise, answer in the same \
                  language as the provided text.";
    let user = if text.trim().is_empty() {
        format!("Instruction: {instruction}\n\nWrite the text now.")
    } else {
        let text: String = text.chars().take(MAX_ASSIST_INPUT_CHARS).collect();
        format!("Instruction: {instruction}\n\nText:\n\"\"\"\n{text}\n\"\"\"")
    };
    vec![
        ChatMessage {
            role: "system",
            content: system.into(),
        },
        ChatMessage {
            role: "user",
            content: user,
        },
    ]
}

impl Core {
    /// Apply `instruction` to `text` (or write new text when `text` is empty)
    /// and return the model's answer.
    pub async fn ai_assist(&self, instruction: String, text: String) -> Result<String> {
        let mut cfg = self.ai_config(Scenario::Draft).await?;
        // The input's own language wins; the UI language only applies to
        // generated text that has no source.
        if !text.trim().is_empty() {
            cfg.language = None;
        }
        let messages = ai::apply_language(assist_prompt(&instruction, &text), &cfg);
        let answer = ai::chat(&cfg, messages).await?;
        Ok(ai::strip_reasoning(&answer).trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_carries_instruction_and_text() {
        let messages = assist_prompt("Shorten", "Hello there");
        assert_eq!(messages.len(), 2);
        assert!(messages[1].content.contains("Shorten"));
        assert!(messages[1].content.contains("Hello there"));
        let draft = assist_prompt("Write a thank-you note", "  ");
        assert!(!draft[1].content.contains("Text:"));
    }
}
