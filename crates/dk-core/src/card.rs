//! 卡片：一行一卡的解析、渲染与自校验。
//!
//! 行格式（见 SPEC §2.1）：
//!
//! ```text
//! <path>@<h8>[:tags]: F:<...> | R:<...> | A:<...> | S:<...> #<v8>
//! ```
//!
//! `<v8>` 是它之前全部字节的 blake3 前 8 位，用来发现截断与手改。

use std::fmt;

/// 字段标识，顺序固定。
pub const FIELDS: [char; 4] = ['F', 'R', 'A', 'S'];

/// 一条卡片。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub path: String,
    /// 源码内容 blake3 前 8 位（小写 hex）。
    pub src_hash: String,
    /// AOCI 兼容位，dk 不解析其语义。
    pub tags: Option<String>,
    pub f: String,
    pub r: String,
    pub a: String,
    pub s: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MissingHash,
    BadHash(String),
    MissingPath,
    BadPath(String),
    MissingFirstField,
    ChecksumMismatch { expected: String, actual: String },
    FieldOrder(char),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MissingHash => write!(f, "missing trailing checksum"),
            ParseError::BadHash(h) => write!(f, "trailing checksum is not 8 hex chars: {h:?}"),
            ParseError::MissingPath => write!(f, "missing path before '@'"),
            ParseError::BadPath(p) => write!(f, "path must not contain '@' or ':' : {p:?}"),
            ParseError::MissingFirstField => write!(f, "card body must start with 'F:'"),
            ParseError::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "checksum mismatch: line says {expected}, computed {actual}"
                )
            }
            ParseError::FieldOrder(c) => write!(f, "unexpected field marker {c:?}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// 8 位小写 hex 的 blake3 前缀。
pub fn short_hash(bytes: &[u8]) -> String {
    let h = blake3::hash(bytes);
    let mut out = String::with_capacity(8);
    for b in &h.as_bytes()[..4] {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn is_hex8(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Card {
    /// 渲染除行尾校验和之外的正文。
    pub fn body(&self) -> String {
        let mut out = String::with_capacity(96);
        out.push_str(&self.path);
        out.push('@');
        out.push_str(&self.src_hash);
        if let Some(t) = &self.tags {
            out.push('[');
            out.push_str(t);
            out.push(']');
        }
        out.push(':');
        out.push_str(" F:");
        out.push_str(&self.f);
        out.push_str(" | R:");
        out.push_str(&self.r);
        out.push_str(" | A:");
        out.push_str(&self.a);
        out.push_str(" | S:");
        out.push_str(&self.s);
        out
    }

    /// 渲染完整行，含行尾校验和。
    pub fn render(&self) -> String {
        let body = self.body();
        format!("{body} #{}", short_hash(body.as_bytes()))
    }

    /// 解析一行。**校验和不符即报错**，不做容错——坏行必须被看见。
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let line = line.trim_end_matches(['\r', '\n']);

        // 1. 行尾校验和：最后一个 " #" 之后必须恰好是 8 位 hex
        let sep = line.rfind(" #").ok_or(ParseError::MissingHash)?;
        let body = &line[..sep];
        let sum = &line[sep + 2..];
        if !is_hex8(sum) {
            return Err(ParseError::BadHash(sum.to_string()));
        }
        let actual = short_hash(body.as_bytes());
        if actual != sum {
            return Err(ParseError::ChecksumMismatch {
                expected: sum.to_string(),
                actual,
            });
        }
        Self::parse_body(body)
    }

    /// 解析不含行尾校验和的正文。`parse` 校验过 checksum 后调它；`dk check --fix` 也用它——
    /// 从「#hash 失配但正文本身合法」的坏行里救回字段，重签一个正确的 #hash。
    pub fn parse_body(body: &str) -> Result<Self, ParseError> {
        // 2. path@h8[:tags]:
        let at = body.find('@').ok_or(ParseError::MissingPath)?;
        let path = &body[..at];
        if path.is_empty() {
            return Err(ParseError::MissingPath);
        }
        if path.contains(':') || path.contains('@') {
            return Err(ParseError::BadPath(path.to_string()));
        }
        let rest = &body[at + 1..];
        let colon = rest.find(':').ok_or(ParseError::MissingFirstField)?;
        let head = &rest[..colon];
        let fields = &rest[colon + 1..];

        let (src_hash, tags) = match head.find('[') {
            Some(lb) => {
                let rb = head
                    .rfind(']')
                    .ok_or_else(|| ParseError::BadHash(head.to_string()))?;
                (head[..lb].to_string(), Some(head[lb + 1..rb].to_string()))
            }
            None => (head.to_string(), None),
        };
        if !is_hex8(&src_hash) {
            return Err(ParseError::BadHash(src_hash));
        }

        // 3. 四段：按 " | " 切，非 X: 开头的段回拼给上一段
        let mut cur: Option<char> = None;
        let mut buf = [String::new(), String::new(), String::new(), String::new()];
        let mut seen = [false; 4];
        for (i, chunk) in fields.split(" | ").enumerate() {
            let trimmed = chunk.trim_start();
            let mut tag = None;
            if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
                let c = trimmed.chars().next().unwrap().to_ascii_uppercase();
                if FIELDS.contains(&c) {
                    tag = Some(c);
                }
            }
            match tag {
                Some(c) => {
                    let idx = FIELDS.iter().position(|x| *x == c).unwrap();
                    if i == 0 && c != 'F' {
                        return Err(ParseError::MissingFirstField);
                    }
                    if seen[idx] {
                        return Err(ParseError::FieldOrder(c));
                    }
                    seen[idx] = true;
                    cur = Some(c);
                    buf[idx] = trimmed[2..].trim().to_string();
                }
                None => match cur {
                    Some(c) => {
                        let idx = FIELDS.iter().position(|x| *x == c).unwrap();
                        buf[idx].push_str(" | ");
                        buf[idx].push_str(chunk);
                    }
                    None => return Err(ParseError::MissingFirstField),
                },
            }
        }
        if !seen[0] {
            return Err(ParseError::MissingFirstField);
        }

        let dash = |s: String| if s.is_empty() { "-".to_string() } else { s };
        Ok(Card {
            path: path.to_string(),
            src_hash,
            tags,
            f: dash(buf[0].clone()),
            r: dash(buf[1].clone()),
            a: dash(buf[2].clone()),
            s: dash(buf[3].clone()),
        })
    }

    /// 源码内容变了没有。
    pub fn is_stale(&self, source: &[u8]) -> bool {
        short_hash(source) != self.src_hash
    }

    /// 单卡总字符数（用于软门告警，见 SPEC §8）。
    pub fn char_len(&self) -> usize {
        self.body().chars().count()
    }

    pub fn r_paths(&self) -> Vec<&str> {
        if self.r == "-" {
            return Vec::new();
        }
        self.r
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Card {
        Card {
            path: "crates/dk-core/src/card.rs".into(),
            src_hash: "8f3a21bc".into(),
            tags: Some("CG7T".into()),
            f: "一行一卡的解析与渲染".into(),
            r: "crates/dk-core/src/store.rs".into(),
            a: "Card::parse / Card::render".into(),
            s: "-".into(),
        }
    }

    #[test]
    fn roundtrip() {
        let c = sample();
        let line = c.render();
        let back = Card::parse(&line).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn rejects_tampering() {
        let mut line = sample().render();
        line = line.replace("一行一卡", "两行一卡");
        assert!(matches!(
            Card::parse(&line),
            Err(ParseError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn keeps_pipe_inside_field() {
        let mut c = sample();
        c.s = "左移 a | 右移 b 都必须同时调整".into();
        let back = Card::parse(&c.render()).unwrap();
        assert_eq!(back.s, "左移 a | 右移 b 都必须同时调整");
    }

    #[test]
    fn missing_tail_hash() {
        assert!(matches!(
            Card::parse("a.rs@8f3a21bc: F:x"),
            Err(ParseError::MissingHash)
        ));
    }

    #[test]
    fn empty_field_becomes_dash() {
        let c = Card {
            path: "x.rs".into(),
            src_hash: "deadbeef".into(),
            tags: None,
            f: "f".into(),
            r: String::new(),
            a: String::new(),
            s: String::new(),
        };
        let back = Card::parse(&c.render()).unwrap();
        assert_eq!(back.r, "-");
        assert_eq!(back.a, "-");
        assert_eq!(back.tags, None);
    }
}
