//! 分词：不判语言，判字符 Script。
//!
//! 三条轨（SPEC §4.1）：
//! - 有空格分隔的文字：UAX#29 词边界取整词
//! - 无空格的文字（中日韩、泰、老挝、高棉）：相邻二元组
//! - 代码标识符：camelCase / snake_case 拆词
//!
//! 没有语言检测、没有停用词表、没有词干化。BM25 的 IDF 会压低高频虚词。

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// 当前分词规则版本。规则一改就升版本，`map.txt` 首行盖章随之失效并重建内存倒排。
pub const TOKENIZER_VERSION: &str = "v1";

pub fn is_unspaced(c: char) -> bool {
    matches!(c as u32,
        0x0E00..=0x0E7F   // Thai
        | 0x0E80..=0x0EFF // Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0x1100..=0x11FF // Hangul Jamo
        | 0x2E80..=0x2EFF // CJK Radicals
        | 0x3040..=0x30FF // Hiragana + Katakana
        | 0x3400..=0x4DBF // CJK Ext A
        | 0x4E00..=0x9FFF // CJK Unified
        | 0xAC00..=0xD7AF // Hangul Syllables
        | 0xF900..=0xFAFF // CJK Compatibility
        | 0x20000..=0x2FA1F // CJK Ext B..F
    )
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// 把标识符拆成有意义的词：refreshEventId -> refresh, event, id
fn split_ident(w: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in w.split(|c: char| !c.is_alphanumeric()) {
        if part.len() < 2 {
            continue;
        }
        let chars: Vec<char> = part.chars().collect();
        let mut cur = String::new();
        for (i, &c) in chars.iter().enumerate() {
            if i > 0 && c.is_uppercase() {
                let prev_lower = chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit();
                let next_lower = chars.get(i + 1).map(|n| n.is_lowercase()).unwrap_or(false);
                if (prev_lower || (chars[i - 1].is_uppercase() && next_lower)) && !cur.is_empty() {
                    out.push(cur.to_lowercase());
                    cur = String::new();
                }
            }
            cur.push(c);
        }
        if cur.len() >= 2 {
            out.push(cur.to_lowercase());
        }
    }
    out
}

/// 切词。返回已小写、已 NFKC 的词元序列（可含重复）。
pub fn tokenize(text: &str) -> Vec<String> {
    let norm: String = text.nfkc().collect();
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut unspaced_run = String::new();

    let flush_buf = |buf: &mut String, out: &mut Vec<String>| {
        if buf.is_empty() {
            return;
        }
        let whole: String = buf.to_lowercase();
        if whole.chars().count() >= 2 {
            out.push(whole.clone());
        }
        for w in split_ident(buf) {
            if w != whole {
                out.push(w);
            }
        }
        buf.clear();
    };

    let flush_unspaced = |run: &mut String, out: &mut Vec<String>| {
        let chars: Vec<char> = run.chars().collect();
        if chars.is_empty() {
            return;
        }
        if chars.len() == 1 {
            out.push(chars[0].to_lowercase().to_string());
        } else {
            for w in chars.windows(2) {
                out.push(format!("{}{}", w[0], w[1]).to_lowercase());
            }
        }
        run.clear();
    };

    for c in norm.chars() {
        if is_unspaced(c) {
            flush_buf(&mut buf, &mut out);
            unspaced_run.push(c);
        } else if is_ident(c) {
            flush_unspaced(&mut unspaced_run, &mut out);
            buf.push(c);
        } else {
            flush_buf(&mut buf, &mut out);
            flush_unspaced(&mut unspaced_run, &mut out);
        }
    }
    flush_buf(&mut buf, &mut out);
    flush_unspaced(&mut unspaced_run, &mut out);

    // 非 ASCII 的分隔文字（希腊、西里尔、阿拉伯…）用 UAX#29 兜一遍
    for w in norm.unicode_words() {
        if w.chars().any(|c| !c.is_ascii() && !is_unspaced(c)) && w.chars().count() >= 2 {
            out.push(w.to_lowercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(v: &[String], s: &str) -> bool {
        v.iter().any(|x| x == s)
    }

    #[test]
    fn cjk_bigrams() {
        let t = tokenize("召回验证两层");
        assert!(has(&t, "召回"));
        assert!(has(&t, "回验"));
        assert!(has(&t, "两层"));
    }

    #[test]
    fn ident_split() {
        let t = tokenize("refreshEventId");
        assert!(has(&t, "refresh"));
        assert!(has(&t, "event"));
        assert!(has(&t, "id"));
        let t2 = tokenize("build_safe_inventory");
        assert!(has(&t2, "build"));
        assert!(has(&t2, "inventory"));
    }

    #[test]
    fn mixed() {
        let t = tokenize("修改 store.rs 的 CAS 写入");
        assert!(has(&t, "store"));
        assert!(has(&t, "cas"));
        assert!(has(&t, "写入"));
    }

    #[test]
    fn cyrillic() {
        let t = tokenize("хранилище файлов");
        assert!(t.iter().any(|x| x == "хранилище"));
    }
}
