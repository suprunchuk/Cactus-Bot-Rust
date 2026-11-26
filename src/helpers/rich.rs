use std::error::Error;
use std::fmt::{self, Display, Write};
use std::sync::LazyLock;
use std::time::Duration;

use serde_json::{Value, json};
use teloxide::Bot;
use teloxide::payloads::AnswerCallbackQuerySetters;
use teloxide::requests::Requester;
use teloxide::types::{CallbackQuery, MaybeInaccessibleMessage};

use crate::config::env;
use crate::constants::{CONNECT_TIMEOUT, REQUEST_TIMEOUT};
use crate::db::DbResult;

pub fn esc(value: impl Display) -> String {
    let mut out = String::new();
    let _ = write!(EscWriter { out: &mut out }, "{value}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_escapes_all_special_chars() {
        assert_eq!(esc("a<&>\"b"), "a&lt;&amp;&gt;&quot;b");
    }

    #[test]
    fn esc_keeps_plain_text_and_multibyte_utf8() {
        assert_eq!(esc("Кактус Макс 🌵"), "Кактус Макс 🌵");
    }

    #[test]
    fn esc_escapes_around_multibyte_chars() {
        assert_eq!(esc("🌵&🌴<x"), "🌵&amp;🌴&lt;x");
    }

    #[test]
    fn esc_works_with_any_display_type() {
        assert_eq!(esc(42), "42");
    }
}

struct EscWriter<'a> {
    out: &'a mut String,
}

impl Write for EscWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        escape_into(s, self.out);
        Ok(())
    }
}

fn escape_into(s: &str, out: &mut String) {
    let mut copied = 0;
    for (i, byte) in s.bytes().enumerate() {
        let replacement = match byte {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            _ => continue,
        };
        out.push_str(&s[copied..i]);
        out.push_str(replacement);
        copied = i + 1;
    }
    out.push_str(&s[copied..]);
}

pub fn btn(label: &str, data: &str) -> String {
    format!(
        "<tg-button type=\"callback_data\" data=\"{}\">{}</tg-button>",
        esc(data),
        esc(label)
    )
}

pub fn btn_styled(label: &str, data: &str, style: &str) -> String {
    format!(
        "<tg-button type=\"callback_data\" data=\"{}\" style=\"{}\">{}</tg-button>",
        esc(data),
        style,
        esc(label)
    )
}

pub fn row(buttons: &[String]) -> String {
    format!("<tg-button-row>{}</tg-button-row>", buttons.join(""))
}

pub fn row_aligned(buttons: &[String], align: &str) -> String {
    format!(
        "<tg-button-row align=\"{}\">{}</tg-button-row>",
        align,
        buttons.join("")
    )
}

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("Не удалось создать HTTP-клиент для Telegram API")
});

async fn post_bot_api(method: &str, payload: Value) -> Result<Value, Box<dyn Error + Send + Sync>> {
    let url = format!("{}/{}", env::get().bot_api_base, method);
    let max_attempts = env::get().telegram_max_retry_attempts;
    let fallback_delay = env::get().telegram_max_retry_delay_seconds;

    let mut attempt: u32 = 0;
    loop {
        let response = HTTP.post(&url).json(&payload).send().await?;
        let status = response.status();
        let text = response.text().await?;
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

        if body["ok"].as_bool() == Some(true) {
            return Ok(body["result"].clone());
        }

        let error = api_error(method, status, &body);
        let retry_after = body["parameters"]["retry_after"].as_u64();

        let retryable = retry_after.is_some() || status.is_server_error();
        if !retryable || attempt >= max_attempts {
            return Err(error.into());
        }

        attempt += 1;
        tracing::warn!(method, attempt, "Telegram API retry");
        tokio::time::sleep(Duration::from_secs(retry_after.unwrap_or(fallback_delay))).await;
    }
}

fn api_error(method: &str, status: reqwest::StatusCode, body: &Value) -> String {
    let description = body["description"]
        .as_str()
        .map_or_else(|| format!("HTTP {status}"), ToOwned::to_owned);
    format!("Telegram API error in {method}: {description}")
}

pub async fn send_rich(chat_id: i64, html: &str) -> Result<Value, Box<dyn Error + Send + Sync>> {
    post_bot_api(
        "sendRichMessage",
        json!({
            "chat_id": chat_id,
            "rich_message": { "html": html },
        }),
    )
    .await
}

pub async fn edit_rich(
    chat_id: i64,
    message_id: i64,
    html: &str,
) -> Result<Value, Box<dyn Error + Send + Sync>> {
    post_bot_api(
        "editMessageText",
        json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "rich_message": { "html": html },
        }),
    )
    .await
}

pub async fn answer_callback(
    bot: &Bot,
    cq: &CallbackQuery,
    text: Option<&str>,
    alert: bool,
) -> DbResult<()> {
    let mut req = bot.answer_callback_query(cq.id.clone());
    if let Some(t) = text {
        req = req.text(t).show_alert(alert);
    }
    req.await?;
    Ok(())
}

pub async fn answer_and_edit(
    bot: &Bot,
    cq: &CallbackQuery,
    text: Option<&str>,
    alert: bool,
    chat_id: i64,
    html: &str,
) -> DbResult<()> {
    let (answer, edit) = tokio::join!(answer_callback(bot, cq, text, alert), async {
        match cq_message_id(cq) {
            Some(message_id) => edit_rich(chat_id, message_id, html).await.map(|_| ()),
            None => Ok(()),
        }
    },);
    answer?;
    edit
}

pub fn cq_chat_id(cq: &CallbackQuery) -> Option<i64> {
    cq.message.as_ref().map(|m| match m {
        MaybeInaccessibleMessage::Regular(msg) => msg.chat.id.0,
        MaybeInaccessibleMessage::Inaccessible(msg) => msg.chat.id.0,
    })
}

pub fn cq_message_id(cq: &CallbackQuery) -> Option<i64> {
    cq.message.as_ref().and_then(|m| match m {
        MaybeInaccessibleMessage::Regular(msg) => Some(i64::from(msg.id.0)),
        MaybeInaccessibleMessage::Inaccessible(_) => None,
    })
}
