//! Microsoft Graph 邮件同步客户端（同步调用，适合在后台线程中使用）。

use crate::SyncError;
use crate::oauth::{OutlookConfig, Tokens, refresh};
use serde_json::Value;

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
}
