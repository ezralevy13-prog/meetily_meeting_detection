//! Q&A chat over a meeting transcript.
//!
//! Reuses the summary pipeline's LLM client, so chat works with every
//! configured provider -- including the fully local Ollama and Built-in AI
//! paths -- without new model plumbing.

use crate::database::repositories::{meeting::MeetingsRepository, setting::SettingsRepository};
use crate::ollama::metadata::ModelMetadataCache;
use crate::state::AppState;
use crate::summary::llm_client::{generate_summary, LLMProvider};
use log::info;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::{AppHandle, Manager, Runtime};

// Same 5-minute TTL as the summary service's metadata cache.
static CHAT_METADATA_CACHE: Lazy<ModelMetadataCache> =
    Lazy::new(|| ModelMetadataCache::new(Duration::from_secs(300)));

/// Most recent chat turns forwarded to the model for follow-up questions.
const MAX_HISTORY_MESSAGES: usize = 12;
/// Cap on any single history message forwarded to the model.
const MAX_HISTORY_MESSAGE_CHARS: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingChatMessage {
    /// "user" or "assistant"
    pub role: String,
    pub content: String,
}

/// Answer a question about a meeting, grounded in its stored transcript.
///
/// `messages` is the chat history in order; the last entry must be the
/// user's current question. Provider and model come from the caller (the
/// frontend's summary-model configuration), while API keys and endpoints
/// are resolved from settings the same way summary generation does.
#[tauri::command]
pub async fn api_chat_with_meeting<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    provider: String,
    model: String,
    messages: Vec<MeetingChatMessage>,
) -> Result<String, String> {
    let question = messages
        .last()
        .filter(|m| m.role == "user")
        .map(|m| m.content.trim().to_string())
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "The last chat message must be a non-empty user question".to_string())?;

    info!(
        "api_chat_with_meeting: meeting_id={}, provider={}, model={}, history_len={}",
        meeting_id,
        provider,
        model,
        messages.len().saturating_sub(1)
    );

    let pool = state.db_manager.pool().clone();

    let meeting = MeetingsRepository::get_meeting(&pool, &meeting_id)
        .await
        .map_err(|e| format!("Failed to load meeting: {}", e))?
        .ok_or_else(|| format!("Meeting {} not found", meeting_id))?;

    // Build a timestamped transcript. Timestamps let the model answer
    // "when did we talk about X" questions.
    let mut transcript = String::new();
    for t in &meeting.transcripts {
        let text = t.text.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(start) = t.audio_start_time {
            transcript.push_str(&format!("[{}] ", format_timestamp(start)));
        }
        transcript.push_str(text);
        transcript.push('\n');
    }
    if transcript.trim().is_empty() {
        return Err("This meeting has no transcript to chat about".to_string());
    }

    let llm_provider = LLMProvider::from_str(&provider)?;

    // Resolve API key / endpoints the same way the summary service does.
    let api_key = match llm_provider {
        LLMProvider::Ollama | LLMProvider::BuiltInAI | LLMProvider::CustomOpenAI => String::new(),
        _ => SettingsRepository::get_api_key(&pool, &provider)
            .await
            .map_err(|e| format!("Failed to retrieve API key for {}: {}", provider, e))?
            .filter(|k| !k.is_empty())
            .ok_or_else(|| format!("No API key configured for {}", provider))?,
    };

    let ollama_endpoint = if llm_provider == LLMProvider::Ollama {
        SettingsRepository::get_model_config(&pool)
            .await
            .ok()
            .flatten()
            .and_then(|c| c.ollama_endpoint)
    } else {
        None
    };

    let (custom_openai_endpoint, custom_api_key, custom_max_tokens, custom_temperature, custom_top_p) =
        if llm_provider == LLMProvider::CustomOpenAI {
            let config = SettingsRepository::get_custom_openai_config(&pool)
                .await
                .map_err(|e| format!("Failed to retrieve custom OpenAI config: {}", e))?
                .ok_or_else(|| "Custom OpenAI provider selected but not configured".to_string())?;
            (
                Some(config.endpoint),
                config.api_key,
                config.max_tokens.map(|t| t as u32),
                config.temperature,
                config.top_p,
            )
        } else {
            (None, None, None, None, None)
        };

    let api_key = if llm_provider == LLMProvider::CustomOpenAI {
        custom_api_key.unwrap_or_default()
    } else {
        api_key
    };

    // Budget the transcript to the model's context window. Local models are
    // the tight case; cloud models get a generous fixed cap. ~4 chars/token.
    let max_transcript_chars: usize = match llm_provider {
        LLMProvider::Ollama => {
            match CHAT_METADATA_CACHE
                .get_or_fetch(&model, ollama_endpoint.as_deref())
                .await
            {
                // Reserve ~1500 tokens for the system prompt, history, and answer.
                Ok(metadata) => metadata
                    .context_size
                    .saturating_sub(1500)
                    .saturating_mul(4)
                    .max(8_000),
                Err(_) => 16_000,
            }
        }
        LLMProvider::BuiltInAI => 24_000,
        _ => 100_000,
    };
    let transcript = truncate_transcript(&transcript, max_transcript_chars);

    let system_prompt = format!(
        "You are Meetily's meeting assistant. Answer the user's questions about the meeting \
         \"{}\" using ONLY the transcript below.\n\
         \n\
         Rules:\n\
         - Ground every answer in the transcript; quote or paraphrase what was actually said.\n\
         - If the transcript does not contain the answer, say so plainly instead of guessing.\n\
         - Lines may start with a timestamp like [12:34] marking when they were said; use \
           timestamps when the user asks when something happened.\n\
         - Be concise and direct.\n\
         \n\
         TRANSCRIPT:\n\
         {}",
        meeting.title, transcript
    );

    // Flatten prior turns into the user prompt (the shared client takes a
    // single system + user prompt pair rather than a message list).
    let mut user_prompt = String::new();
    let history = &messages[..messages.len() - 1];
    if !history.is_empty() {
        user_prompt.push_str("Conversation so far:\n");
        let start = history.len().saturating_sub(MAX_HISTORY_MESSAGES);
        for m in &history[start..] {
            let role = if m.role == "assistant" { "Assistant" } else { "User" };
            let mut content = m.content.trim().to_string();
            if content.len() > MAX_HISTORY_MESSAGE_CHARS {
                let mut end = MAX_HISTORY_MESSAGE_CHARS;
                while !content.is_char_boundary(end) {
                    end -= 1;
                }
                content.truncate(end);
                content.push_str("...");
            }
            user_prompt.push_str(&format!("{}: {}\n", role, content));
        }
        user_prompt.push('\n');
    }
    user_prompt.push_str(&format!("Question: {}", question));

    let client = reqwest::Client::new();
    let app_data_dir = app.path().app_data_dir().ok();

    generate_summary(
        &client,
        &llm_provider,
        &model,
        &api_key,
        &system_prompt,
        &user_prompt,
        ollama_endpoint.as_deref(),
        custom_openai_endpoint.as_deref(),
        custom_max_tokens,
        custom_temperature,
        custom_top_p,
        app_data_dir.as_ref(),
        None,
    )
    .await
}

fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}

/// Keep the transcript within the model's context budget. When it doesn't
/// fit, keep the opening (agenda/participants context) and as much of the
/// end as possible, with an explicit marker where the middle was dropped.
fn truncate_transcript(transcript: &str, max_chars: usize) -> String {
    if transcript.len() <= max_chars {
        return transcript.to_string();
    }

    let head_len = max_chars / 4;
    let tail_len = max_chars - head_len;

    let mut head_end = head_len;
    while !transcript.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = transcript.len() - tail_len;
    while !transcript.is_char_boundary(tail_start) {
        tail_start += 1;
    }

    format!(
        "{}\n[... middle of the transcript omitted to fit the model's context window ...]\n{}",
        &transcript[..head_end],
        &transcript[tail_start..]
    )
}
