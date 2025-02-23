use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, InputFile},
    Bot,
    RequestError,
    utils::command::BotCommands,
};
use std::env;
use std::process::Command;
use log::{info, error};
use tokio::fs;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::Duration;

#[tokio::main]
async fn main() {
    env_logger::init();
    dotenv::dotenv().ok();

    let bot_token = env::var("TELOXIDE_TOKEN").expect("TELOXIDE_TOKEN not set in .env");
    let api_url = env::var("TELOXIDE_API_URL")
        .unwrap_or_else(|_| "http://telegram-api:8081/bot".to_string());

    let bot = Bot::new(bot_token).set_api_url(
        api_url.parse().expect("Invalid TELOXIDE_API_URL format"),
    );

    let settings: Arc<Mutex<HashMap<ChatId, String>>> = Arc::new(Mutex::new(HashMap::new()));

    info!("Starting Telegram YouTube Bot with local API at {}", api_url);

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter_command::<Commands>()
                .endpoint(handle_commands),
        )
        .branch(
            Update::filter_message()
                .filter(|msg: Message| {
                    msg.text().map_or(false, |text| {
                        text.starts_with("/download ") || text.starts_with("/audio ")
                    })
                })
                .endpoint(handle_download_command),
        )
        .branch(
            Update::filter_message()
                .endpoint(send_welcome),
        )
        .branch(
            Update::filter_callback_query()
                .endpoint(handle_callback),
        );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![settings])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Доступные команды:")]
enum Commands {
    #[command(description = "Показать меню")]
    Start,
    #[command(description = "Выбрать качество по умолчанию")]
    Quality,
}

async fn handle_commands(bot: Bot, msg: Message, cmd: Commands) -> Result<(), RequestError> {
    match cmd {
        Commands::Start => send_welcome(bot, msg).await?,
        Commands::Quality => {
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![
                    InlineKeyboardButton::callback("360p", "quality_360p"),
                    InlineKeyboardButton::callback("720p", "quality_720p"),
                ],
                vec![
                    InlineKeyboardButton::callback("1080p", "quality_1080p"),
                    InlineKeyboardButton::callback("Best", "quality_best"),
                ],
            ]);
            bot.send_message(msg.chat.id, "Выберите качество по умолчанию:")
                .reply_markup(keyboard)
                .await?;
        }
    }
    Ok(())
}

async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    settings: Arc<Mutex<HashMap<ChatId, String>>>,
) -> Result<(), RequestError> {
    if let (Some(data), Some(msg)) = (q.data, q.message) {
        let chat_id = msg.chat.id;
        let quality = match data.as_str() {
            "quality_360p" => "360p",
            "quality_720p" => "720p",
            "quality_1080p" => "1080p",
            "quality_best" => "best",
            _ => return Ok(()),
        };

        let mut settings = settings.lock().await;
        settings.insert(chat_id, quality.to_string());
        bot.send_message(chat_id, format!("Качество по умолчанию установлено: {}", quality))
            .await?;
    }
    Ok(())
}

async fn handle_download_command(
    bot: Bot,
    msg: Message,
    settings: Arc<Mutex<HashMap<ChatId, String>>>,
) -> Result<(), RequestError> {
    if let Some(text) = msg.text() {
        let (url, audio_only) = if text.starts_with("/download ") {
            (text.trim_start_matches("/download ").to_string(), false)
        } else if text.starts_with("/audio ") {
            (text.trim_start_matches("/audio ").to_string(), true)
        } else {
            return Ok(());
        };

        let settings = settings.lock().await;
        let quality = settings.get(&msg.chat.id).cloned().unwrap_or_else(|| "best".to_string());
        handle_download(&bot, msg.chat.id, &url, audio_only, &quality).await?;
    }
    Ok(())
}

async fn send_welcome(bot: Bot, msg: Message) -> Result<(), RequestError> {
    bot.send_message(
        msg.chat.id,
        "Привет! Используй:\n/download <URL> - для видео\n/audio <URL> - для аудио\n/quality - выбрать качество по умолчанию",
    )
    .await?;
    Ok(())
}

async fn handle_download(bot: &Bot, chat_id: ChatId, url: &str, audio_only: bool, quality: &str) -> Result<(), RequestError> {
    let status_msg = bot.send_message(chat_id, "Начинаю загрузку...").await?;

    let format = if audio_only {
        "bestaudio[ext=mp3]"
    } else {
        match quality {
            "360p" => "bestvideo[height<=360]+bestaudio/best[height<=360]",
            "720p" => "bestvideo[height<=720]+bestaudio/best[height<=720]",
            "1080p" => "bestvideo[height<=1080]+bestaudio/best[height<=1080]",
            _ => "bestvideo+bestaudio/best",
        }
    };
    let output_file = if audio_only { "/data/output.mp3" } else { "/data/output.mp4" };

    if !Command::new("which").arg("yt-dlp").status().map_or(false, |s| s.success()) {
        bot.edit_message_text(chat_id, status_msg.id, "Ошибка: yt-dlp не установлен в контейнере")
            .await?;
        error!("yt-dlp not found in container");
        return Ok(());
    }

    let status = Command::new("yt-dlp")
        .arg("-f")
        .arg(format)
        .arg("-o")
        .arg(output_file)
        .arg(url)
        .status()
        .map_err(|e| {
            error!("Failed to execute yt-dlp: {}", e);
            RequestError::RetryAfter(Duration::from_secs(5))
        })?;

    if status.success() {
        let file_path = if audio_only { "/data/output.mp3" } else { "/data/output.mp4" };
        if fs::metadata(file_path).await.is_ok() {
            if audio_only {
                bot.send_audio(chat_id, InputFile::file(file_path)).await?;
            } else {
                bot.send_video(chat_id, InputFile::file(file_path)).await?;
            }
            fs::remove_file(file_path).await.map_err(|e| {
                error!("Failed to remove file {}: {}", file_path, e);
                RequestError::RetryAfter(Duration::from_secs(5))
            })?;
            info!("Successfully downloaded and sent: {} with quality {}", url, quality);
            bot.delete_message(chat_id, status_msg.id).await?;
        } else {
            bot.edit_message_text(chat_id, status_msg.id, "Ошибка: файл не был создан").await?;
            error!("File {} not found after download", file_path);
        }
    } else {
        bot.edit_message_text(chat_id, status_msg.id, "Ошибка при загрузке. Проверь URL или попробуй позже.")
            .await?;
        error!("yt-dlp failed for URL: {} with quality {}", url, quality);
    }

    Ok(())
}
