//! 单路径「受不受管」的完整判定（SPEC §8 硬门）。
//!
//! `sweep::walk` 遍历时只看名字与元数据（十万文件也要快）；`dk_put` 拿到的是一条
//! 外来路径，必须回答更严格的问题：它存在吗、在仓库里吗、walk 会不会收它。
//! 两个入口共用同一套谓词，才不会出现「写得进索引、检索永远看不到」的失联卡。

use crate::ignore_rules::{is_binary_ext, is_builtin_dir, Rules};
use std::fs;
use std::path::Path;

/// 超过此大小的文件不进索引（SPEC §2.2）。
pub const MAX_BYTES: u64 = 1 << 20;

pub enum Verdict {
    /// 受管。`rel` 是以磁盘为准的正名（Windows 上会把大小写纠正过来）。
    Managed { rel: String },
    /// 不受管或压根不存在；字符串是给人看的理由。
    Rejected(String),
}

/// 归一：反斜杠换正斜杠、折叠重复 `/`、丢掉空段与 `.` 段。
/// 前导 `/` 保留——它是绝对路径的标记，由 `verify` 负责拒。
pub fn normalize(raw: &str) -> String {
    let s = raw.trim().replace('\\', "/");
    let abs = s.starts_with('/');
    let segs: Vec<&str> = s
        .split('/')
        .filter(|x| !x.is_empty() && *x != ".")
        .collect();
    let body = segs.join("/");
    if abs {
        format!("/{body}")
    } else {
        body
    }
}

fn shape_error(rel: &str) -> Option<String> {
    if rel.is_empty() || rel == "/" {
        return Some("路径为空".into());
    }
    if rel.starts_with('/') {
        return Some("必须是仓库相对路径，不能是绝对路径".into());
    }
    // 行格式用 `@` 分隔路径与源码哈希、用 `:` 分隔四段；写进去就解析不回来了。
    if rel.contains(':') || rel.contains('@') {
        return Some("路径不能含 ':' 或 '@'（与卡片行格式冲突）".into());
    }
    for seg in rel.split('/') {
        if seg == ".." {
            return Some("路径不能含 '..'".into());
        }
        if is_builtin_dir(seg) {
            return Some(format!("'{seg}' 是内置排除目录"));
        }
    }
    if rel == ".dkignore" {
        return Some(".dkignore 是配置，不是被治理对象".into());
    }
    if is_binary_ext(rel) {
        return Some("已知二进制扩展名".into());
    }
    None
}

/// 一条路径能不能建卡。这是 put 唯一的入口判定。
pub fn verify(root: &Path, rules: &Rules, raw: &str) -> Verdict {
    let rel = normalize(raw);
    if let Some(why) = shape_error(&rel) {
        return Verdict::Rejected(why);
    }
    let Ok(canon_root) = fs::canonicalize(root) else {
        return Verdict::Rejected("仓库根不可读".into());
    };
    // 先 canonicalize 再判越界：这一步同时挡住 `C:/Windows/x`、UNC、以及指向仓库外的链接。
    let Ok(canon) = fs::canonicalize(root.join(&rel)) else {
        return Verdict::Rejected("文件不存在".into());
    };
    if !canon.starts_with(&canon_root) {
        return Verdict::Rejected("解析后落在仓库之外".into());
    }
    let Ok(md) = fs::metadata(&canon) else {
        return Verdict::Rejected("读不到文件属性".into());
    };
    if !md.is_file() {
        return Verdict::Rejected("不是普通文件".into());
    }
    if md.len() > MAX_BYTES {
        return Verdict::Rejected(format!("超过 {MAX_BYTES} 字节，不进索引"));
    }
    let real = canon
        .strip_prefix(&canon_root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    if real.is_empty() {
        return Verdict::Rejected("取不到仓库相对路径".into());
    }
    // 正名之后可能又冒出新问题（真实文件名含 @ 之类），再查一次形态。
    if let Some(why) = shape_error(&real) {
        return Verdict::Rejected(why);
    }
    if rules.is_ignored(&real, false) {
        return Verdict::Rejected("已被 .dkignore 排除".into());
    }
    Verdict::Managed { rel: real }
}

/// 按磁盘正名取仓库相对路径（Windows 大小写、符号链接都纠正过来）；不在库内或不存在给 None。
/// 读路径的工具（dk_get / dk_map 的 paths）必须先过这一步：字符串比较会把
/// `src/Alpha.rs` 判成另一个文件，于是对存在的文件断言「这文件没建卡」。
pub fn canonical_rel(root: &Path, rel: &str) -> Option<String> {
    let cr = fs::canonicalize(root).ok()?;
    let c = fs::canonicalize(root.join(rel)).ok()?;
    let real = c
        .strip_prefix(&cr)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    if real.is_empty() {
        None
    } else {
        Some(real)
    }
}

/// R 里 `code:` 目标的在库判定（SPEC §8 硬门）。不要求受管，只要求真实存在且在仓库内。
pub fn exists_in_repo(root: &Path, raw: &str) -> bool {
    let rel = normalize(raw);
    if rel.is_empty() || rel.starts_with('/') || rel.contains(':') || rel.contains('@') {
        return false;
    }
    if rel.split('/').any(|s| s == "..") {
        return false;
    }
    let Ok(cr) = fs::canonicalize(root) else {
        return false;
    };
    let Ok(c) = fs::canonicalize(root.join(&rel)) else {
        return false;
    };
    c.starts_with(&cr) && c.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(tag);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("src")).unwrap();
        p
    }

    #[test]
    fn normalizes_slashes_and_dots() {
        assert_eq!(normalize("./src/a.rs"), "src/a.rs");
        assert_eq!(normalize("src\\a.rs"), "src/a.rs");
        assert_eq!(normalize("src//a.rs"), "src/a.rs");
        assert_eq!(normalize("/etc/passwd"), "/etc/passwd");
    }

    #[test]
    fn rejects_shapes() {
        let root = tmp("managed_shape");
        let rules = Rules::load(&root).unwrap();
        for bad in [
            "",
            "../x.rs",
            "/etc/passwd",
            "C:/Windows/win.ini",
            "src/a@2x.png",
            "src/a:b.rs",
        ] {
            assert!(
                matches!(verify(&root, &rules, bad), Verdict::Rejected(_)),
                "{bad} 应当被拒"
            );
            let _ = fs::write(root.join("src/a.rs"), "fn a(){}");
            assert!(!exists_in_repo(&root, bad) || bad.is_empty());
        }
    }

    #[test]
    fn accepts_existing_file() {
        let root = tmp("managed_ok");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        let rules = Rules::load(&root).unwrap();
        assert!(matches!(
            verify(&root, &rules, "src/a.rs"),
            Verdict::Managed { .. }
        ));
        assert!(!exists_in_repo(&root, "src/none.rs"));
        assert!(exists_in_repo(&root, "./src/a.rs"));
    }

    #[test]
    fn rejects_oversize_and_ignored() {
        let root = tmp("managed_gate");
        fs::write(root.join("big.rs"), vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        fs::write(root.join("gen.rs"), "fn g(){}").unwrap();
        fs::write(root.join(".dkignore"), "gen.rs\n").unwrap();
        let rules = Rules::load(&root).unwrap();
        assert!(matches!(
            verify(&root, &rules, "big.rs"),
            Verdict::Rejected(_)
        ));
        assert!(matches!(
            verify(&root, &rules, "gen.rs"),
            Verdict::Rejected(_)
        ));
    }
}
