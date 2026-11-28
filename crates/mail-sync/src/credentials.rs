//! 凭据保存：Outlook 的令牌与 Gmail 的应用专用密码存入系统凭据管理器
//! （Windows 凭据管理器、macOS 钥匙串、Linux Secret Service），不写入任何文件。
//!
//! 通过 [`SecretStore`] 抽象，测试时用内存实现。

use crate::SyncError;
use crate::oauth::Tokens;
use std::collections::HashMap;
use std::sync::Mutex;

/// 凭据在系统凭据管理器中的服务名。
pub const SERVICE: &str = "Letterdesk";

/// 保存机密字符串的地方。
pub trait SecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SyncError>;
    fn set(&self, key: &str, value: &str) -> Result<(), SyncError>;
    /// 删除；不存在时不报错。
    fn delete(&self, key: &str) -> Result<(), SyncError>;
}

fn credential_err(e: keyring::Error) -> SyncError {
    SyncError::Credential(e.to_string())
}

/// 系统凭据管理器。
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<String>, SyncError> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(credential_err)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(credential_err(e)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SyncError> {
        keyring::Entry::new(SERVICE, key)
            .and_then(|e| e.set_password(value))
            .map_err(credential_err)
    }

    fn delete(&self, key: &str) -> Result<(), SyncError> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(credential_err)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(credential_err(e)),
        }
    }
}

/// 内存中的存储，用于测试。
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<String>, SyncError> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SyncError> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), SyncError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        Ok(())
    }
}

fn outlook_key(account: &str) -> String {
    format!("outlook:{}", account.trim().to_lowercase())
}

fn gmail_key(account: &str) -> String {
    format!("gmail:{}", account.trim().to_lowercase())
}

/// 保存 Outlook 令牌。刷新令牌每次使用后都会更换，同步结束后应再次保存。
pub fn save_outlook_tokens(
    store: &dyn SecretStore,
    account: &str,
    tokens: &Tokens,
) -> Result<(), SyncError> {
    let json = serde_json::to_string(tokens).map_err(|e| SyncError::Credential(e.to_string()))?;
    store.set(&outlook_key(account), &json)
}

pub fn load_outlook_tokens(
    store: &dyn SecretStore,
    account: &str,
) -> Result<Option<Tokens>, SyncError> {
    match store.get(&outlook_key(account))? {
        Some(json) => Ok(serde_json::from_str(&json).ok()),
        None => Ok(None),
    }
}

pub fn save_gmail_password(
    store: &dyn SecretStore,
    account: &str,
    password: &str,
) -> Result<(), SyncError> {
    store.set(&gmail_key(account), password)
}

pub fn load_gmail_password(
    store: &dyn SecretStore,
    account: &str,
) -> Result<Option<String>, SyncError> {
    store.get(&gmail_key(account))
}

/// 删除某个账号保存的全部凭据。
pub fn forget(store: &dyn SecretStore, account: &str) -> Result<(), SyncError> {
    store.delete(&outlook_key(account))?;
    store.delete(&gmail_key(account))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_passwords_round_trip_per_account() {
        let store = MemoryStore::default();
        let tokens = Tokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 42,
        };
        assert_eq!(
            load_outlook_tokens(&store, "Lin@Example.com").unwrap(),
            None
        );
        save_outlook_tokens(&store, "Lin@Example.com", &tokens).unwrap();
        // 地址不区分大小写。
        assert_eq!(
            load_outlook_tokens(&store, "lin@example.com").unwrap(),
            Some(tokens)
        );
        save_gmail_password(&store, "lin@example.com", "abcd efgh ijkl mnop").unwrap();
        assert_eq!(
            load_gmail_password(&store, "LIN@example.com")
                .unwrap()
                .as_deref(),
            Some("abcd efgh ijkl mnop")
        );
        assert_eq!(
            load_gmail_password(&store, "other@example.com").unwrap(),
            None
        );
        forget(&store, "lin@example.com").unwrap();
        assert_eq!(
            load_outlook_tokens(&store, "lin@example.com").unwrap(),
            None
        );
        assert_eq!(
            load_gmail_password(&store, "lin@example.com").unwrap(),
            None
        );
        // 删除不存在的凭据不报错。
        forget(&store, "lin@example.com").unwrap();
    }

    #[test]
    fn corrupted_token_entry_is_treated_as_missing() {
        let store = MemoryStore::default();
        store.set(&outlook_key("x@y.z"), "not json").unwrap();
        assert_eq!(load_outlook_tokens(&store, "x@y.z").unwrap(), None);
    }
}
