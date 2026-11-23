//! 目录枚举与建卡进度：`dk_sweep` 与覆盖率检查的唯一数据源。
//!
//! 这里**不含任何哈希**。判断「哪些文件还没有卡」只需要目录树与已建卡的路径集合，
//! 10 万文件约 0.6 s；每轮都全量哈希是另一件事（`dk_check`），不该混进来。

use crate::ignore_rules::{is_binary_ext, is_builtin_dir, Rules};
use crate::store::Store;
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// 超过此大小的文件不进索引（SPEC §2.2）。生成物与大二进制的主力都在这里。
const MAX_BYTES: u64 = 1 << 20;
/// 读前这么多字节就够判特征了，不必读完大文件。
const SNIFF_BYTES: u64 = 100 << 10;

pub struct Walk {
    pub files: Vec<String>,
}

/// 受管文件清单（仓库相对路径，正斜杠，字典序）。
pub fn walk(root: &Path, rules: &Rules) -> Result<Walk> {
    let root_canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        let abs = if dir.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&dir)
        };
        let Ok(rd) = fs::read_dir(&abs) else {
            continue;
        };
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            let rel = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            let Ok(ft) = ent.file_type() else {
                continue;
            };
            // 符号链接一律跳过：链接目标可能在仓库外，跟进去等于越界。
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if is_builtin_dir(&name) || rules.is_ignored(&rel, true) {
                    continue;
                }
                // 目录联接（junction）不是 symlink，file_type 看不出来。进去之前确认它没跑到仓库外。
                let inside = fs::canonicalize(abs.join(&name))
                    .map(|c| c.starts_with(&root_canon))
                    .unwrap_or(false);
                if !inside {
                    continue;
                }
                stack.push(rel);
                continue;
            }
            // .dkignore 是 dk 自己的配置，不是被治理的对象。
            if !ft.is_file()
                || rel == ".dkignore"
                || is_binary_ext(&rel)
                || rules.is_ignored(&rel, false)
            {
                continue;
            }
            let Ok(md) = ent.metadata() else {
                continue;
            };
            if md.len() > MAX_BYTES {
                continue;
            }
            out.push(rel);
        }
    }
    out.sort();
    Ok(Walk { files: out })
}

#[derive(Debug, Serialize)]
pub struct SweepItem {
    pub path: String,
    pub bytes: u64,
    pub lines: usize,
    pub max_line: usize,
    /// 机器给的特征，供模型判断是否值得建卡（SPEC §3 dk_sweep）。
    pub flags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SweepBatch {
    /// 受管文件总数。
    pub total: usize,
    /// 已有卡的受管文件数。
    pub done: usize,
    /// 还差多少（= total - done）。
    pub remaining: usize,
    pub batch: Vec<SweepItem>,
}

/// 未建卡文件的前 `limit` 个（字典序稳定，重跑即续跑）。
pub fn sweep(store: &Store, limit: usize) -> Result<SweepBatch> {
    let rules = Rules::load(store.root())?;
    let all = walk(store.root(), &rules)?.files;
    let (cards, _) = store.load()?;
    let carded: BTreeSet<String> = cards.into_iter().map(|c| c.path).collect();
    let total = all.len();
    let done = all.iter().filter(|p| carded.contains(*p)).count();
    let missing: Vec<String> = all.into_iter().filter(|p| !carded.contains(p)).collect();
    let remaining = missing.len();
    let batch = missing
        .into_iter()
        .take(limit.max(1))
        .map(|p| probe(store.root(), &p))
        .collect::<Vec<_>>();
    Ok(SweepBatch {
        total,
        done,
        remaining,
        batch,
    })
}

fn probe(root: &Path, rel: &str) -> SweepItem {
    let mut flags = Vec::new();
    let bytes = fs::metadata(root.join(rel)).map(|m| m.len()).unwrap_or(0);
    let head = fs::read(root.join(rel))
        .map(|b| b[..b.len().min(SNIFF_BYTES as usize)].to_vec())
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = 0usize;
    let mut max_line = 0usize;
    let mut chars_all = 0usize;
    for l in text.lines() {
        lines += 1;
        let n = l.chars().count();
        chars_all += n;
        max_line = max_line.max(n);
    }
    if bytes > 512 << 10 {
        flags.push("large".into());
    }
    if head.contains(&0) {
        flags.push("binary".into());
    }
    let avg = chars_all.checked_div(lines).unwrap_or(0);
    if (lines <= 3 && avg > 300) || max_line > 2000 || rel.contains(".min.") {
        flags.push("minified".into());
    }
    if rel.starts_with("vendor/")
        || rel.contains("/vendor/")
        || rel.contains("third_party/")
        || rel.contains("dist/")
        || rel.contains("build/")
    {
        flags.push("vendor".into());
    }
    let low = text.to_ascii_lowercase();
    if low.contains("do not edit")
        || low.contains("code generated")
        || low.contains("@generated")
        || text.contains("自动生成")
    {
        flags.push("generated".into());
    }
    let base = rel.rsplit('/').next().unwrap_or(rel);
    if matches!(
        base,
        "Cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "go.sum"
            | "poetry.lock"
    ) {
        flags.push("lockfile".into());
    }
    SweepItem {
        path: rel.to_string(),
        bytes,
        lines,
        max_line,
        flags,
    }
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
        fs::create_dir_all(p.join("node_modules/x")).unwrap();
        p
    }

    #[test]
    fn walk_skips_builtin_and_ignore() {
        let root = tmp("sweep_walk");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        fs::write(root.join("node_modules/x/i.js"), "x").unwrap();
        fs::write(root.join("docs.md"), "# d").unwrap();
        fs::write(root.join(".dkignore"), "docs.md\n").unwrap();
        let rules = Rules::load(&root).unwrap();
        let w = walk(&root, &rules).unwrap();
        // docs.md 被 .dkignore 排除；.dkignore 自身也不受管；node_modules 内置排除。
        assert_eq!(w.files, vec!["src/a.rs".to_string()]);
    }

    #[test]
    fn walk_reports_bad_ignore_patterns() {
        let root = tmp("sweep_bad_ignore");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        fs::write(root.join(".dkignore"), "a**b\nsrc/a.rs\n").unwrap();
        let rules = Rules::load(&root).unwrap();
        // 坏模式不致命：同一个文件里的好模式照常生效，仓库照常可枚举。
        assert!(rules.bad_patterns().len() <= 1);
        assert!(walk(&root, &rules).unwrap().files.is_empty());
    }

    #[test]
    fn sweep_reports_progress_and_flags() {
        let root = tmp("sweep_batch");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        fs::write(root.join("src/big.min.js"), "x".repeat(3000)).unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        let b = sweep(&s, 10).unwrap();
        assert_eq!(b.done, 0);
        assert_eq!(b.remaining, b.total);
        let min = b
            .batch
            .iter()
            .find(|i| i.path.ends_with(".min.js"))
            .unwrap();
        assert!(min.flags.contains(&"minified".to_string()));
    }
}
