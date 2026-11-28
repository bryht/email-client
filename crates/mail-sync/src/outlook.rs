//! Microsoft Graph 邮件同步客户端（同步调用，适合在后台线程中使用）。

use crate::SyncError;
use crate::compose::ReplyDraft;
use crate::oauth::{OutlookConfig, Tokens, refresh};
use crate::send::{SendReport, SendStatus};
use serde_json::{Value, json};

/// Graph 的 ID 是不透明的字符串，作为单个 URL 路径段传送。
fn message_path_id(id: &str) -> percent_encoding::PercentEncode<'_> {
    const ESCAPE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    percent_encoding::utf8_percent_encode(id, ESCAPE)
}

/// 一封邮件的摘要（来自 delta 结果）。
#[derive(Debug, Clone, PartialEq)]
pub struct MessageSummary {
    pub id: String,
    pub internet_message_id: Option<String>,
    pub subject: String,
    pub is_read: bool,
    pub received: String,
}

/// 一个文件夹自上次同步以来的变化。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FolderChanges {
    /// 新增或有变化的邮件。
    pub upserted: Vec<MessageSummary>,
    /// 已删除的邮件 id。
    pub removed: Vec<String>,
    /// 下次增量同步的起点。
    pub delta_link: String,
}

pub struct OutlookClient {
    config: OutlookConfig,
    tokens: Tokens,
    agent: ureq::Agent,
    page_size: usize,
}

impl OutlookClient {
    pub fn new(config: OutlookConfig, tokens: Tokens) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            config,
            tokens,
            agent,
            page_size: 50,
        }
    }

    /// 每页条数（`Prefer: odata.maxpagesize`）。
    pub fn with_page_size(mut self, n: usize) -> Self {
        self.page_size = n.max(1);
        self
    }

    /// 当前令牌。刷新后会变化，调用方需要保存。
    pub fn tokens(&self) -> &Tokens {
        &self.tokens
    }

    fn refresh_tokens(&mut self) -> Result<(), SyncError> {
        self.tokens = refresh(&self.config, &self.tokens)?;
        Ok(())
    }

    /// GET 请求；访问令牌过期（或返回 401）时刷新一次再重试。
    fn get(&mut self, url: &str) -> Result<ureq::http::Response<ureq::Body>, SyncError> {
        if self.tokens.is_expired() {
            self.refresh_tokens()?;
        }
        for attempt in 0..2 {
            let res = self
                .agent
                .get(url)
                .header(
                    "Authorization",
                    &format!("Bearer {}", self.tokens.access_token),
                )
                .header("Prefer", &format!("odata.maxpagesize={}", self.page_size))
                .call()
                .map_err(|e| SyncError::Network(e.to_string()))?;
            if res.status().as_u16() == 401 && attempt == 0 {
                self.refresh_tokens()?;
                continue;
            }
            return Ok(res);
        }
        unreachable!()
    }

    fn get_json(&mut self, url: &str) -> Result<Value, SyncError> {
        let mut res = self.get(url)?;
        let status = res.status().as_u16();
        let body: Value = res
            .body_mut()
            .read_json()
            .map_err(|e| SyncError::Protocol(e.to_string()))?;
        if status != 200 {
            let code = body["error"]["code"].as_str().unwrap_or("unknown");
            return Err(SyncError::Protocol(format!("Graph 返回 {status}：{code}")));
        }
        Ok(body)
    }

    /// 当前用户的 (显示名, 邮箱地址)。
    pub fn me(&mut self) -> Result<(String, String), SyncError> {
        let url = format!("{}/v1.0/me", self.config.graph);
        let me = self.get_json(&url)?;
        let mail = me["mail"]
            .as_str()
            .or(me["userPrincipalName"].as_str())
            .unwrap_or_default();
        Ok((
            me["displayName"].as_str().unwrap_or_default().to_string(),
            mail.to_string(),
        ))
    }

    /// 增量同步一个文件夹（`inbox`、`sentitems`、`drafts`）。
    /// `delta_link` 为上次返回的值；为空时做首次同步。
    pub fn sync_folder(
        &mut self,
        folder: &str,
        delta_link: Option<&str>,
    ) -> Result<FolderChanges, SyncError> {
        let mut url = match delta_link {
            Some(link) => link.to_string(),
            None => format!(
                "{}/v1.0/me/mailFolders/{folder}/messages/delta?$select=id,internetMessageId,subject,isRead,receivedDateTime",
                self.config.graph
            ),
        };
        let mut changes = FolderChanges::default();
        loop {
            let page = self.get_json(&url)?;
            for item in page["value"].as_array().into_iter().flatten() {
                let Some(id) = item["id"].as_str() else {
                    continue;
                };
                if item.get("@removed").is_some() {
                    changes.removed.push(id.to_string());
                } else {
                    changes.upserted.push(MessageSummary {
                        id: id.to_string(),
                        internet_message_id: item["internetMessageId"].as_str().map(String::from),
                        subject: item["subject"].as_str().unwrap_or_default().to_string(),
                        is_read: item["isRead"].as_bool().unwrap_or(false),
                        received: item["receivedDateTime"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    });
                }
            }
            if let Some(next) = page["@odata.nextLink"].as_str() {
                url = next.to_string();
            } else if let Some(delta) = page["@odata.deltaLink"].as_str() {
                changes.delta_link = delta.to_string();
                return Ok(changes);
            } else {
                return Err(SyncError::Protocol(
                    "delta 响应既没有 nextLink 也没有 deltaLink".into(),
                ));
            }
        }
    }

    /// 下载一封邮件的 MIME 原文。
    pub fn download(&mut self, id: &str) -> Result<Vec<u8>, SyncError> {
        let id = message_path_id(id);
        let url = format!("{}/v1.0/me/messages/{id}/$value", self.config.graph);
        let mut res = self.get(&url)?;
        if res.status().as_u16() != 200 {
            return Err(SyncError::Protocol(format!(
                "下载邮件 {id} 失败：{}",
                res.status()
            )));
        }
        res.body_mut()
            .read_to_vec()
            .map_err(|e| SyncError::Network(e.to_string()))
    }

    /// POST / PATCH 请求（JSON 请求体可选）；返回状态码和 JSON（没有内容时为 Null）。
    /// 返回 401 时刷新令牌并重试一次。
    fn call(
        &mut self,
        method: &str,
        url: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), SyncError> {
        if self.tokens.is_expired() {
            self.refresh_tokens()?;
        }
        for attempt in 0..2 {
            let auth = format!("Bearer {}", self.tokens.access_token);
            let result = match (method, body) {
                ("PATCH", Some(b)) => self
                    .agent
                    .patch(url)
                    .header("Authorization", &auth)
                    .send_json(b),
                ("PATCH", None) => self
                    .agent
                    .patch(url)
                    .header("Authorization", &auth)
                    .send_empty(),
                (_, Some(b)) => self
                    .agent
                    .post(url)
                    .header("Authorization", &auth)
                    .send_json(b),
                (_, None) => self
                    .agent
                    .post(url)
                    .header("Authorization", &auth)
                    .send_empty(),
            };
            let mut res = result.map_err(|e| SyncError::Network(e.to_string()))?;
            let status = res.status().as_u16();
            if status == 401 && attempt == 0 {
                self.refresh_tokens()?;
                continue;
            }
            let text = res.body_mut().read_to_string().unwrap_or_default();
            let value = serde_json::from_str(&text).unwrap_or(Value::Null);
            return Ok((status, value));
        }
        unreachable!()
    }

    fn recipients(list: &[mail_core::Mailbox]) -> Value {
        Value::Array(
            list.iter()
                .map(|m| json!({ "emailAddress": { "name": m.display_name(), "address": m.address } }))
                .collect(),
        )
    }

    /// 回复全部：先用 `createReplyAll` 生成带线程信息的草稿，写入用户确认的收件人和正文，再发送。
    /// `original_id` 是原信的 Graph id。
    pub fn reply_all(
        &mut self,
        original_id: &str,
        draft: &ReplyDraft,
    ) -> Result<SendReport, SyncError> {
        let original_id = message_path_id(original_id);
        let base = format!("{}/v1.0/me/messages", self.config.graph);
        let (status, created) = self.call(
            "POST",
            &format!("{base}/{original_id}/createReplyAll"),
            Some(&json!({})),
        )?;
        if status != 201 && status != 200 {
            return Err(SyncError::Protocol(format!(
                "无法生成回复草稿：{status} {}",
                created["error"]["code"]
            )));
        }
        let draft_id = created["id"].as_str().unwrap_or_default().to_string();
        let message_id = created["internetMessageId"]
            .as_str()
            .unwrap_or_default()
            .trim_matches(['<', '>'])
            .to_string();
        let patch = json!({
            "toRecipients": Self::recipients(&draft.to),
            "ccRecipients": Self::recipients(&draft.cc),
            "body": { "contentType": "HTML", "content": draft.html },
        });
        let (status, body) = self.call(
            "PATCH",
            &format!("{base}/{}", message_path_id(&draft_id)),
            Some(&patch),
        )?;
        if status != 200 {
            return Err(SyncError::Protocol(format!(
                "无法保存回复草稿：{status} {}",
                body["error"]["code"]
            )));
        }
        let status = self.send_draft(&draft_id, &message_id);
        Ok(SendReport {
            message_id,
            draft_id: Some(draft_id),
            status,
        })
    }

    /// 发送一份草稿；失败或没有确认时到“已发送”核对。也用于“重试”。
    pub fn send_draft(&mut self, draft_id: &str, message_id: &str) -> SendStatus {
        let draft_id = message_path_id(draft_id);
        let url = format!("{}/v1.0/me/messages/{draft_id}/send", self.config.graph);
        let error = match self.call("POST", &url, None) {
            Ok((202, _)) | Ok((200, _)) => return SendStatus::Sent,
            Ok((status, body)) if status < 500 => {
                return SendStatus::NotSent(format!(
                    "服务器拒绝发送：{status} {}",
                    body["error"]["code"]
                ));
            }
            Ok((status, _)) => format!("服务器返回 {status}"),
            Err(e) => e.to_string(),
        };
        match self.find_sent(message_id) {
            Ok(true) => SendStatus::SentConfirmedAfterError,
            Ok(false) => SendStatus::NotSent(error),
            Err(e) => SendStatus::MaybeSent(format!("{error}；核对“已发送”也失败：{e}")),
        }
    }

    /// “已发送”中是否有这封邮件。
    pub fn find_sent(&mut self, message_id: &str) -> Result<bool, SyncError> {
        let filter = format!("internetMessageId eq '<{message_id}>'");
        let encoded: String = form_urlencoded::byte_serialize(filter.as_bytes()).collect();
        let url = format!(
            "{}/v1.0/me/mailFolders/sentitems/messages?$filter={encoded}&$select=id",
            self.config.graph
        );
        let body = self.get_json(&url)?;
        Ok(body["value"].as_array().is_some_and(|v| !v.is_empty()))
    }
}
