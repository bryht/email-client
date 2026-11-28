use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("找不到命令 `{0}`，请确认已安装并在 PATH 中")]
    NotInstalled(String),

    #[error("{tool} 无法满足安全限制，未开放：{reason}")]
    Unsafe { tool: String, reason: String },

    #[error("{tool} 运行超过 {timeout:?}，已终止")]
    Timeout { tool: String, timeout: Duration },

    #[error("{tool} 运行失败（退出码 {code:?}）：{detail}")]
    Failed {
        tool: String,
        code: Option<i32>,
        detail: String,
    },

    #[error("模型输出不符合约定格式，已丢弃：{0}")]
    InvalidOutput(String),

    #[error("{0}")]
    Config(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
