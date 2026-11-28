use serde::{Deserialize, Serialize};
use std::fmt;

/// 一个邮件地址及其显示名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mailbox {
    pub name: Option<String>,
    pub address: String,
}

impl Mailbox {
    pub fn new(name: Option<&str>, address: &str) -> Self {
        Self {
            name: name
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(String::from),
            address: address.trim().to_string(),
        }
    }

    /// 用于比较的地址形式：小写。
    pub fn normalized(&self) -> String {
        normalize(&self.address)
    }

    /// 显示名；没有时用地址。
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.address)
    }
}

impl fmt::Display for Mailbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{name} <{}>", self.address),
            None => write!(f, "{}", self.address),
        }
    }
}

/// 地址比较时统一转为小写。
pub(crate) fn normalize(address: &str) -> String {
    address.trim().to_lowercase()
}
