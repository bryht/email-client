//! 对话归组（PLAN 第 2 节“新来的一封信放到哪儿”）。
//!
//! 规则：
//! 1. **回复**（In-Reply-To / References 指向已知邮件）且不是转发：跟随父邮件进入同一对话，
//!    中途增减 Cc 也不拆分。
//! 2. **转发**：一律新开对话，并记录来源对话；对转发的后续回复按规则 1 留在新对话。
//! 3. **另起的新邮件**（不是回复，或回复的邮件不在本地）：按直接参与人（发件人和 To，
//!    去掉自己及别名）匹配已有对话；只增减 Cc 时沿用同一对话。
//!
//! 补充规则：自己只在 Cc 里时，同样按发件人和 To 归组；发件人与 Reply-To 不同时按发件人；
//! 同一个人的多个地址不合并。
//!
//! 对话内再按话题（标准邮件线程）分组：同一线程的邮件共享根邮件，AI 只读取对应线程。
//!
//! 归组是纯函数：给定全部邮件和自己的地址，结果确定，便于每次同步后整体重算。

use mail_core::{Email, Mailbox};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};

/// 一个对话：与同一个人或同一组人的往来。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conversation {
    pub id: usize,
    /// 直接参与人（不含自己），按地址排序。
    pub participants: Vec<Mailbox>,
    /// 按时间排序的邮件下标（对应输入切片）。
    pub messages: Vec<usize>,
    /// 对话内的话题。
    pub threads: Vec<Thread>,
    /// 由转发产生时，来源邮件所在的对话。
    pub forwarded_from: Option<usize>,
    /// 最近一封邮件的时间戳。
    pub last_timestamp: Option<i64>,
}

/// 对话中的一个话题（标准邮件线程）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Thread {
    /// 线程根邮件的 Message-ID；没有时为合成的标识。
    pub root: String,
    /// 去掉 Re:、Fwd: 等前缀后的主题。
    pub subject: String,
    pub messages: Vec<usize>,
}

fn normalize(address: &str) -> String {
    address.trim().to_lowercase()
}

/// 去掉主题前面的回复、转发前缀（可重复出现）。
pub fn base_subject(subject: &str) -> String {
    const PREFIXES: [&str; 13] = [
        "re:",
        "fwd:",
        "fw:",
        "aw:",
        "wg:",
        "tr:",
        "sv:",
        "回复:",
        "回复：",
        "答复:",
        "答复：",
        "转发:",
        "转发：",
    ];
    let mut s = subject.trim();
    while let Some(p) = PREFIXES.iter().find(|p| s.to_lowercase().starts_with(*p)) {
        s = s[p.len()..].trim_start();
    }
    s.to_string()
}

/// 判断一封邮件是不是转发：主题以转发前缀开头，或正文带有常见的转发分隔行。
pub fn is_forward(email: &Email) -> bool {
    let subject = email.subject.trim().to_lowercase();
    let subject_forward = ["fwd:", "fw:", "wg:", "tr:", "转发:", "转发："]
        .iter()
        .any(|p| subject.starts_with(p));
    let body_forward = email.body.lines().any(|l| {
        let l = l.trim().to_lowercase();
        l.starts_with("---------- forwarded message")
            || l.starts_with("begin forwarded message")
            || l.starts_with("-------- 转发的邮件")
    });
    subject_forward || body_forward
}

/// 直接参与人：发件人和 To，去掉自己（含别名）与重复地址。自己发出的邮件只算 To。
fn direct_participants(email: &Email, me: &BTreeSet<String>) -> (BTreeSet<String>, Vec<Mailbox>) {
    let mut keys = BTreeSet::new();
    let mut boxes = Vec::new();
    for m in std::iter::once(&email.from).chain(email.to.iter()) {
        let key = normalize(&m.address);
        if key.is_empty() || me.contains(&key) || !keys.insert(key) {
            continue;
        }
        boxes.push(m.clone());
    }
    boxes.sort_by_key(|m| normalize(&m.address));
    (keys, boxes)
}

/// 把邮件归组成对话。`me` 是自己的地址和别名。结果按最近往来时间倒序排列。
pub fn group(emails: &[Email], me: &[String]) -> Vec<Conversation> {
    let me: BTreeSet<String> = me.iter().map(|a| normalize(a)).collect();

    // 按时间处理，保证父邮件先于回复（时间缺失的放最后，保持原顺序）。
    let mut order: Vec<usize> = (0..emails.len()).collect();
    order.sort_by_key(|&i| (emails[i].timestamp.is_none(), emails[i].timestamp, i));

    // Message-ID → (邮件下标)
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for (i, e) in emails.iter().enumerate() {
        if let Some(id) = &e.message_id {
            by_id.entry(id.clone()).or_insert(i);
        }
    }

    let mut conv_of: Vec<Option<usize>> = vec![None; emails.len()];
    let mut convs: Vec<Conversation> = Vec::new();
    // 直接参与人集合 → 由“新邮件”规则建立的对话。
    let mut by_participants: HashMap<BTreeSet<String>, usize> = HashMap::new();

    let new_conversation =
        |convs: &mut Vec<Conversation>, participants: Vec<Mailbox>, from: Option<usize>| {
            let id = convs.len();
            convs.push(Conversation {
                id,
                participants,
                messages: Vec::new(),
                threads: Vec::new(),
                forwarded_from: from,
                last_timestamp: None,
            });
            id
        };

    for &i in &order {
        let email = &emails[i];
        // 已处理过的父邮件（In-Reply-To 优先，其次 References 从后往前）。
        let parent = email
            .in_reply_to
            .iter()
            .chain(email.references.iter().rev())
            .filter_map(|id| by_id.get(id).copied())
            .find(|&p| p != i && conv_of[p].is_some());

        let conv = if is_forward(email) {
            let source = parent.and_then(|p| conv_of[p]);
            let (_, boxes) = direct_participants(email, &me);
            new_conversation(&mut convs, boxes, source)
        } else if let Some(p) = parent {
            conv_of[p].expect("parent processed")
        } else {
            let (keys, boxes) = direct_participants(email, &me);
            match by_participants.get(&keys) {
                Some(&c) => c,
                None => {
                    let c = new_conversation(&mut convs, boxes, None);
                    by_participants.insert(keys, c);
                    c
                }
            }
        };
        conv_of[i] = Some(conv);
        let c = &mut convs[conv];
        c.messages.push(i);
        c.last_timestamp = c.last_timestamp.max(email.timestamp);
    }

    // 对话内按线程分组：根为 References 的第一项、In-Reply-To，或邮件自身。
    for c in &mut convs {
        let mut threads: Vec<Thread> = Vec::new();
        let mut root_of: HashMap<String, usize> = HashMap::new();
        for &i in &c.messages {
            let e = &emails[i];
            let root = e
                .references
                .first()
                .or(e.in_reply_to.first())
                .cloned()
                .or_else(|| e.message_id.clone())
                .unwrap_or_else(|| format!("local-{i}"));
            // 线程根可能就是本对话里另一封邮件所在的线程。
            let existing = root_of.get(&root).copied().or_else(|| {
                e.in_reply_to
                    .iter()
                    .chain(e.references.iter())
                    .filter_map(|id| root_of.get(id).copied())
                    .next()
            });
            let t = match existing {
                Some(t) => t,
                None => {
                    threads.push(Thread {
                        root: root.clone(),
                        subject: base_subject(&e.subject),
                        messages: Vec::new(),
                    });
                    threads.len() - 1
                }
            };
            threads[t].messages.push(i);
            root_of.insert(root, t);
            if let Some(id) = &e.message_id {
                root_of.insert(id.clone(), t);
            }
        }
        c.threads = threads;
    }

    convs.sort_by(|a, b| {
        b.last_timestamp
            .cmp(&a.last_timestamp)
            .then(a.id.cmp(&b.id))
    });
    convs
}

#[cfg(test)]
mod tests;
