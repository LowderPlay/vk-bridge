use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use vkclient::{List, Version, VkApi, VkApiResult};
use vkclient::longpoll::LongPollRequest;
use futures_util::StreamExt;
use serde_json::{json, Value};
use teloxide::adaptors::DefaultParseMode;
use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::{InputFile, InputMedia, InputMediaAudio, InputMediaPhoto, InputMediaVideo, LinkPreviewOptions, MessageId, ParseMode, Recipient, ReplyParameters};
use url::Url;
use regex::{Captures, Regex};

#[derive(Serialize)]
struct MessagesLongPoll {
    lp_version: u8,
}

#[derive(Deserialize)]
struct LongPollResponse {
    key: String,
    server: String,
    ts: u64,
}

enum MessageType {
    Text(MessageId),
    Caption(MessageId)
}

#[derive(Clone)]
struct State {
    api: VkApi,
    bot: DefaultParseMode<Bot>,
    chat_map: HashMap<u64, Recipient>,
    msg_map: Arc<Mutex<HashMap<u64, MessageType>>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let vk_token = std::env::var("VK_TOKEN").expect("pass VK_TOKEN as env variable");
    let tg_token = std::env::var("BOT_TOKEN").expect("pass BOT_TOKEN as env variable");
    let mut chat_map: HashMap<u64, Recipient> = serde_json::from_str(&std::fs::read_to_string("chats.json")?)?;
    println!("{:#?}", chat_map);

    let client: VkApi = vkclient::VkApiBuilder::new(vk_token.to_string()).with_version(Version(5, 199)).into();

    let bot = Bot::new(tg_token).parse_mode(ParseMode::MarkdownV2);

    let msg_map = HashMap::<u64, MessageType>::new();

    let state = State {
        api: client.clone(),
        bot,
        chat_map,
        msg_map: Arc::new(Mutex::new(msg_map)),
    };

    loop {
        let LongPollResponse { key, server, ts } = client.send_request("messages.getLongPollServer", MessagesLongPoll {
            lp_version: 3
        }).await?;

        let stream = client.longpoll().subscribe::<_, Value>(LongPollRequest {
            server,
            key,
            ts: ts.to_string(),
            wait: 25,
            additional_params: json!({"mode": 2, "version": 3}),
        });

        let state = state.clone();
        stream
            .for_each(move |r| {
                let state = state.clone();
                async move {
                    if let Ok(v) = r {
                        handle_msg(&state, v).await;
                    }
                }
            }).await;
    }
}

async fn handle_msg(state: &State, msg: Value) {
    match msg.get(0).and_then(|v| v.as_i64()) {
        Some(4) => new_message(state, msg).await,
        Some(5) => edit_message(state, msg).await,
        _ => {}
    }
}

async fn format_message(state: &State, id: u64, from: &str, text: &str) -> (String, Vec<Attachment>) {
    let from = get_sender(&state.api, from).await.unwrap_or("???".to_string());
    let (attachments, attach, action) = handle_attach(state, id).await;
    let text = if let Some(action) = action {
        action
    } else {
        let re = Regex::new("\\[(id\\d+|club\\d+)\\|(.+)]").unwrap();
        let mut mentions = vec![];
        let text = re.replace_all(&text, |groups: &Captures| {
            mentions.push(format!("[{}](https://vk.com/{})",
                                  groups.get(2).unwrap().as_str(),
                                  groups.get(1).unwrap().as_str()));
            "\x01"
        }).to_string();
        let mut text = markdown_escape(text.to_string());
        for mention in mentions {
            text = text.replacen("\x01", &mention, 1);
        }
        text
    };
    let msg = format!("*{}*\n{}{}\n{}", from, text, if text.len() == 0 { "" } else { "\n" }, attach);

    return (msg, attachments);
}

fn markdown_escape(s: String) -> String {
    html_escape::decode_html_entities(&s.replace("\\", "\\\\")
        .replace("<br>", "\n"))
        .replace("_", "\\_")
        .replace("*", "\\*")
        .replace("[", "\\[")
        .replace("]", "\\]")
        .replace("(", "\\(")
        .replace(")", "\\)")
        .replace("~", "\\~")
        .replace("`", "\\`")
        .replace(">", "\\>")
        .replace("#", "\\#")
        .replace("+", "\\+")
        .replace("-", "\\-")
        .replace("=", "\\=")
        .replace("|", "\\|")
        .replace("{", "\\{")
        .replace("}", "\\}")
        .replace(".", "\\.")
        .replace("!", "\\!")
}

async fn new_message(state: &State, msg: Value) {
    let id = msg.get(1).unwrap().as_u64().unwrap();
    let chat_id = msg.get(3).unwrap().as_u64().unwrap();
    if chat_id < 2000000000 {
        return;
    }
    let text = msg.get(5).unwrap().as_str().unwrap();
    let from = msg.get(6).unwrap().get("from").unwrap().as_str().unwrap();

    if let Some(chat) = state.chat_map.get(&chat_id) {
        println!("new message #{} to chat {} from {}", id, chat_id, from);
        let reply = if let Some(reply) = msg.get(7).unwrap().get("reply") {
            let reply: Value = serde_json::from_str(reply.as_str().unwrap()).unwrap();
            let mid = reply.get("conversation_message_id").unwrap().as_u64().unwrap();

            let resp: GetMessageResponse = state.api.send_request("messages.getByConversationMessageId", GetMessageByConvIdRequest {
                conversation_message_ids: List(vec![mid]),
                peer_id: chat_id
            }).await.unwrap();
            let msg: VkMessage = resp.items.into_iter().nth(0).unwrap();

            if let Some(MessageType::Caption(id) | MessageType::Text(id)) = state.msg_map.lock().unwrap().get(&msg.id) {
                Some(ReplyParameters {
                    message_id: id.clone(),
                    chat_id: None,
                    allow_sending_without_reply: None,
                    quote: None,
                    quote_parse_mode: None,
                    quote_entities: None,
                    quote_position: None,
                })
            } else {
                None
            }
        } else {
            None
        };

        let (msg, attachments) = format_message(state, id, from, text).await;

        let mut media = vec![];
        for (i, attach) in attachments.into_iter().enumerate() {
            match attach {
                Attachment::Photo { file } => {
                    let mut photo = InputMediaPhoto::new(file);
                    if i == 0 {
                        photo = photo.caption(msg.clone());
                    }
                    media.push(InputMedia::Photo(photo));
                }
                Attachment::Voice { file } => {
                    let mut voice = InputMediaAudio::new(file);
                    if i == 0 {
                        voice = voice.caption(msg.clone());
                    }
                    media.push(InputMedia::Audio(voice));
                }
                Attachment::Video { file} => {
                    let mut video = InputMediaVideo::new(file);
                    if i == 0 {
                        video = video.caption(msg.clone());
                    }
                    media.push(InputMedia::Video(video));
                }
            }
        }

        if media.len() > 0 {
            let mut msg = state.bot.send_media_group(chat.clone(), media);
            if let Some(reply) = reply {
                msg = msg.reply_parameters(reply);
            }
            let msg = msg.await.unwrap().into_iter().nth(0).unwrap();
            state.msg_map.lock().unwrap().insert(id, MessageType::Caption(msg.id));
        } else {
            let mut msg = state.bot.send_message(chat.clone(), msg)
                .link_preview_options(LinkPreviewOptions {
                    is_disabled: true,
                    url: None,
                    prefer_small_media: false,
                    prefer_large_media: false,
                    show_above_text: false,
                });
            if let Some(reply) = reply {
                msg = msg.reply_parameters(reply);
            }
            let msg = msg.await.unwrap();
            state.msg_map.lock().unwrap().insert(id, MessageType::Text(msg.id));
        };
        // println!("{:#?}", msg);
    }
}

#[derive(Serialize)]
struct GetMessageRequest {
    message_ids: List<Vec<u64>>,
    fields: List<Vec<String>>,
    preview_length: usize,
}

#[derive(Serialize)]
struct GetMessageByConvIdRequest {
    peer_id: u64,
    conversation_message_ids: List<Vec<u64>>,
}

#[derive(Deserialize, Debug)]
struct VkMessage {
    id: u64,
    conversation_message_id: u64,
    text: String,
    peer_id: u64,
    attachments: Vec<VkAttachment>,
    fwd_messages: Vec<VkFwdMessage>,
    action: Option<VkMessageAction>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum VkMessageAction {
    ChatPhotoUpdate,
    ChatPhotoRemove,
    ChatCreate,
    ChatTitleUpdate,
    ChatInviteUser,
    ChatKickUser,
    ChatPinMessage,
    ChatUnpinMessage,
    ChatInviteUserByLink
}

#[derive(Deserialize, Debug)]
struct VkFwdMessage {
    id: Option<u64>,
    conversation_message_id: u64,
    text: String,
    from_id: i64,
    peer_id: Option<u64>,
    attachments: Vec<VkAttachment>,
    fwd_messages: Option<Vec<VkFwdMessage>>,
}


#[derive(Deserialize, Debug)]
struct VkPhotoFile {
    height: usize,
    width: usize,
    url: String,
}

#[derive(Deserialize, Debug)]
struct VkPhoto {
    orig_photo: VkPhotoFile
}

#[derive(Deserialize, Debug)]
struct VkVideo {
    files: Value,
    player: String,
}

#[derive(Deserialize, Debug)]
struct VkDocument {
    title: String,
    url: String,
    ext: Option<String>,
    preview: Option<VkDocumentPreview>
}

#[derive(Deserialize, Debug)]
struct VkDocumentPreview {
    video: Option<Value>
}

#[derive(Deserialize, Debug)]
struct VkAudioMessage {
    link_mp3: String,
    link_ogg: String,
}

#[derive(Deserialize, Debug)]
struct VkPoll {
    question: String,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PostAuthor {
    Profile { first_name: String, last_name: String },
    Group { name: String },
    #[serde(other)]
    Unknown,
}
impl Default for PostAuthor {
    fn default() -> Self {
        PostAuthor::Unknown
    }
}


#[derive(Deserialize, Debug)]
struct VkWallPost {
    to_id: i64,
    id: i64,
    #[serde(default = "PostAuthor::default")]
    from: PostAuthor,
}

#[derive(Deserialize, Debug)]
struct VkSticker {
    sticker_id: i64,
    product_id: i64,
}

#[derive(Deserialize, Debug)]
struct VkLink {
    url: String,
    title: String,
    caption: String,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum VkAttachment {
    Photo { photo: VkPhoto },
    Video { video: VkVideo },
    Doc { doc: VkDocument },
    AudioMessage { audio_message: VkAudioMessage },
    Poll { poll: VkPoll },
    Wall { wall: VkWallPost },
    Sticker { sticker: VkSticker },
    Link { link: VkLink },
    #[serde(other)]
    Unsupported,
}

#[derive(Deserialize)]
struct GetMessageResponse {
    count: usize,
    items: Vec<VkMessage>
}

enum Attachment {
    Photo { file: InputFile },
    Voice { file: InputFile },
    Video { file: InputFile },
}

async fn handle_attach(state: &State, id: u64) -> (Vec<Attachment>, String, Option<String>) {
    let resp: GetMessageResponse = state.api.send_request("messages.getById", GetMessageRequest {
        message_ids: List(vec![id]),
        fields: List(vec!["name".to_string()]),
        preview_length: 1
    }).await.unwrap();
    let mut attachments = vec![];
    let mut texts = vec![];

    let msg = resp.items.into_iter().nth(0).unwrap();
    if let Some(action) = msg.action {
        return (attachments, "".to_string(), Some(match action {
            VkMessageAction::ChatPhotoUpdate => "_изменил\\(а\\) фотографию_",
            VkMessageAction::ChatPhotoRemove => "_удалил\\(а\\) фотографию_",
            VkMessageAction::ChatCreate => "_создал\\(а\\) чат_",
            VkMessageAction::ChatTitleUpdate => "_обновил\\(а\\) название чата_",
            VkMessageAction::ChatInviteUser => "_пригласил\\(а\\) пользователя_",
            VkMessageAction::ChatKickUser => "_исключил\\(а\\) пользователя_",
            VkMessageAction::ChatPinMessage => "_закрепил\\(а\\) сообщение_",
            VkMessageAction::ChatUnpinMessage => "_открепил\\(а\\) сообщение_",
            VkMessageAction::ChatInviteUserByLink => "_присоединился по ссылке_",
        }.to_string()))
    }
    for attach in &msg.attachments {
        match attach {
            VkAttachment::Photo { photo } => {
                attachments.push(Attachment::Photo {
                    file: InputFile::url(Url::parse(&photo.orig_photo.url).unwrap())
                });
            }
            VkAttachment::Unsupported => {
                texts.push("Вложение не поддерживается".to_string())
            }
            VkAttachment::Sticker { sticker } => {
                attachments.push(Attachment::Photo {
                    file: InputFile::url(Url::parse(&format!("https://vk.com/sticker/1-{}-128b", sticker.sticker_id)).unwrap()),
                })
            }
            VkAttachment::Doc { doc } => {
                if let Some(VkDocumentPreview { video: Some(video) }) = &doc.preview {
                    attachments.push(Attachment::Video {
                        file: InputFile::url(Url::parse(&video.get("src").unwrap()
                            .as_str().unwrap()).unwrap()),
                    })
                }
                texts.push(format!("[{}]({})", markdown_escape(doc.title.to_string()), doc.url))
            }
            VkAttachment::AudioMessage { audio_message } => {
                attachments.push(Attachment::Voice {
                    file: InputFile::url(Url::parse(&audio_message.link_ogg).unwrap()),
                })
            }
            VkAttachment::Video { video } => {
                if let Some(url) = video.files.get("mp4_720")
                    .or(video.files.get("mp4_480"))
                    .or(video.files.get("mp4_360"))
                    .or(video.files.get("mp4_240"))
                    .or(video.files.get("mp4_144")) {
                    attachments.push(Attachment::Video {
                        file: InputFile::url(Url::parse(&url.as_str().unwrap()).unwrap()),
                    })
                } else {
                    texts.push(format!("[Видео]({})", video.player).to_string())
                }
            }
            VkAttachment::Poll { poll } => {
                texts.push(format!("📊 _{}_", markdown_escape(poll.question.to_string())).to_string())
            }
            VkAttachment::Wall { wall } => {
                let author = match &wall.from {
                    PostAuthor::Profile { first_name, last_name } => format!("{} {}", first_name, last_name),
                    PostAuthor::Group { name } => name.to_string(),
                    PostAuthor::Unknown => "Неизвестно".to_string(),
                };
                texts.push(format!("[Публикация от {}](https://vk.com/wall{}_{})", markdown_escape(author), wall.to_id, wall.id).to_string())
            }
            VkAttachment::Link { link } => {
                texts.push(format!("Ссылка _[{} \\| {}]({})_",
                                   markdown_escape(link.title.to_string()),
                                   markdown_escape(link.caption.to_string()),
                                   link.url).to_string())
            }
        };
    }

    for fwd in msg.fwd_messages {
        // TODO: add more info
        let sender = get_sender(&state.api, &fwd.from_id.to_string()).await.unwrap_or("???".to_string());
        texts.push(format!("Пересланное сообщение от {}\n>{}||", sender, markdown_escape(fwd.text)
            .split("\n")
            .collect::<Vec<_>>()
            .join("\n>")).to_string());
    }

    let text_attach_count = texts.len();
    let mut attachments_text: String = "🔗 *Вложения*:".to_string();
    for attach in texts {
        attachments_text += &format!("\n{}", attach)
    }

    let attachments_text = if text_attach_count > 0 {
        attachments_text
    } else {
        "".to_string()
    };

    (attachments, attachments_text, None)
}

async fn edit_message(state: &State, msg: Value) {
    let id = msg.get(1).unwrap().as_u64().unwrap();
    let chat = msg.get(3).unwrap().as_u64().unwrap();
    if chat < 2000000000 {
        return;
    }
    let text = msg.get(5).unwrap().as_str().unwrap();
    let from = msg.get(6).unwrap().get("from").unwrap().as_str().unwrap();

    if let (Some(chat), Some(msg)) = (state.chat_map.get(&chat), state.msg_map.lock().unwrap().get(&id)) {
        let (message, _) = format_message(state, id, from, text).await;
        match msg {
            MessageType::Text(id) => {
                let _ = state.bot.edit_message_text(chat.clone(), id.clone(), message).link_preview_options(LinkPreviewOptions {
                    is_disabled: true,
                    url: None,
                    prefer_small_media: false,
                    prefer_large_media: false,
                    show_above_text: false,
                }).await;
            }
            MessageType::Caption(id) => {
                let _ = state.bot.edit_message_caption(chat.clone(), id.clone())
                    .caption(message).await;
            }
        }
    }
}

#[derive(Serialize, Debug)]
struct UsersGetRequest {
    user_ids: List<Vec<usize>>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct UsersGetResponse {
    id: i64,
    first_name: String,
    last_name: String,
}

async fn get_sender(api: &VkApi, from: &str) -> VkApiResult<String> {
    let id: i32 = from.parse().unwrap();
    if id > 0 {
        let user: Vec<UsersGetResponse> = api.send_request("users.get", UsersGetRequest {
            user_ids: List(vec![id as usize]),
        }).await?;
        let user = user.into_iter().nth(0).unwrap();
        Ok(format!("[{} {}](https://vk.com/id{})", markdown_escape(user.first_name),
                   markdown_escape(user.last_name), user.id))
    } else {
        Ok("БОТ".to_string())
    }
}