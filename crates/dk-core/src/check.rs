//! 全量体检（`dk check`，也是 `dk_check` 工具）：唯一会哈希全部源码的地方。
//!
//! 这里慢一点没关系——它不进模型流程，是人的命令。所以不做增量、不维护账本，
//! 每次都是「遍历 + 并行哈希 + 逐卡比对」，结论只依赖磁盘现状。

use crate::ignore_rules::Rules;
use crate::store::Store;
use crate::sweep;
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// 未建卡清单最多列这么多条，其余只给数量（省 token，也避免刷屏）。
const SAMPLE: usize = 20;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// 受管文件总数。
    pub managed: usize,
    pub cards: usize,
    /// 覆盖率是否 100%：无未建卡、无坏卡、无过期。
    pub complete: bool,
    pub stale: Vec<String>,
    /// 卡指向的文件不在了，或已被 .dkignore 排除。
    pub missing: Vec<String>,
    pub uncarded: usize,
    pub uncarded_sample: Vec<String>,
    /// R 里 code: 指向的路径不存在。
    pub bad_r: Vec<String>,
    /// 行号 + 原因（含行尾校验和不符）。
    pub malformed: Vec<String>,
    /// 同一路径出现多张卡（保留先出现者）。
    pub duplicate: Vec<String>,
    /// .dkignore 里解析不了的模式：不致命，但那几行此刻等于不存在。
    pub ignore_warnings: Vec<String>,
    /// AGENTS.md 受管块的现状：present / stale / missing。只提示，不进 is_clean ——
    /// 块是给别的 harness 看的软提醒，缺了不该让老仓库突然「不干净」。
    pub agents_block: String,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.stale.is_empty()
            && self.missing.is_empty()
            && self.bad_r.is_empty()
            && self.malformed.is_empty()
            && self.duplicate.is_empty()
            && self.ignore_warnings.is_empty()
            && self.uncarded == 0
    }

    pub fn summary(&self) -> String {
        let mut s = format!(
            "受管 {} 文件 / {} 张卡 / 覆盖率 {:.1}%",
            self.managed,
            self.cards,
            if self.managed == 0 {
                100.0
            } else {
                self.cards as f64 * 100.0 / self.managed as f64
            }
        );
        for (label, n) in [
            ("过期", self.stale.len()),
            ("失联", self.missing.len()),
            ("未建卡", self.uncarded),
            ("坏 R", self.bad_r.len()),
            ("坏行", self.malformed.len()),
            ("重复", self.duplicate.len()),
            (".dkignore 坏模式", self.ignore_warnings.len()),
        ] {
            if n > 0 {
                s.push_str(&format!(" | {label} {n}"));
            }
        }
        if self.is_clean() {
            s.push_str(" | 一致");
        }
        match self.agents_block.as_str() {
            "stale" => s.push_str(" | AGENTS.md 块是旧版（dk init 可重写）"),
            "missing" => s.push_str(" | 无 AGENTS.md 受管块（dk init 可写入）"),
            "broken" => s.push_str(" | AGENTS.md 的 dk 标记不成对（手工修好再 dk init）"),
            "unreadable" => s.push_str(" | AGENTS.md 读不了（非 UTF-8，或它不是普通文件？）"),
            _ => {}
        }
        s
    }
}

pub fn run(store: &Store) -> Result<Report> {
    let root = store.root();
    let rules = Rules::load(root)?;
    let mut rep = Report {
        ignore_warnings: rules.bad_patterns().to_vec(),
        ..Default::default()
    };
    rep.agents_block = crate::agents::status(root).to_string();

    let (cards, errors) = store.load()?;
    rep.cards = cards.len();
    for (line, e) in errors {
        rep.malformed.push(format!("第 {line} 行：{e}"));
    }

    let files = sweep::walk(root, &rules)?.files;
    rep.managed = files.len();

    // 同一路径多张卡：保留先出现的，后面的算重复。
    let mut by_path: BTreeMap<&str, &crate::card::Card> = BTreeMap::new();
    for c in &cards {
        if by_path.insert(c.path.as_str(), c).is_some() {
            rep.duplicate.push(c.path.clone());
        }
    }

    // 并行哈希：blake3 单文件很快，瓶颈在读 IO，按核数切块即可。
    let hashes = hash_all(root, &files);

    for (rel, card) in &by_path {
        match hashes.get(*rel) {
            Some(h) if h == &card.src_hash => {}
            Some(_) => rep.stale.push((*rel).to_string()),
            None => rep.missing.push(if root.join(rel).exists() {
                format!("{rel}（已被排除但仍有卡）")
            } else {
                format!("{rel}（文件不存在）")
            }),
        }
        for r in card.r_paths() {
            // 只校验 code: 路径；database:// 是外部命名空间，本地无法判定。
            if let Some(p) = r.strip_prefix("code:") {
                if !root.join(p.trim()).exists() {
                    rep.bad_r.push(format!("{rel} -> {}", p.trim()));
                }
            }
        }
    }

    let mut uncarded = Vec::new();
    for f in &files {
        if !by_path.contains_key(f.as_str()) {
            uncarded.push((*f).clone());
        }
    }
    rep.uncarded = uncarded.len();
    rep.uncarded_sample = uncarded.into_iter().take(SAMPLE).collect();
    rep.complete = rep.is_clean();
    Ok(rep)
}

/// 受管文件 -> 内容哈希（前 8 位）。读失败的文件不出现在结果里，按失联处理。
fn hash_all(root: &Path, files: &[String]) -> BTreeMap<String, String> {
    let n = std::thread::available_parallelism()
        .map(|x| x.get())
        .unwrap_or(4)
        .clamp(1, 16);
    let per = files.len().div_ceil(n).max(1);
    let mut out = BTreeMap::new();
    std::thread::scope(|sc| {
        let handles: Vec<_> = files
            .chunks(per)
            .map(|chunk| {
                sc.spawn(move || {
                    let mut local = Vec::with_capacity(chunk.len());
                    for rel in chunk {
                        if let Ok(b) = std::fs::read(root.join(rel)) {
                            local.push((rel.clone(), crate::card::short_hash(&b)));
                        }
                    }
                    local
                })
            })
            .collect();
        for h in handles {
            if let Ok(local) = h.join() {
                out.extend(local);
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Limits, PutEntry, PutOpts};
    use std::fs;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(tag);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("src")).unwrap();
        p
    }

    fn entry(path: &str, f: &str) -> PutEntry {
        PutEntry {
            path: path.into(),
            f: Some(f.into()),
            r: None,
            a: None,
            s: None,
            tags: None,
            skip: false,
        }
    }

    #[test]
    fn clean_after_full_coverage() {
        let root = tmp("check_clean");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        s.put(
            &[entry("src/a.rs", "甲")],
            &Limits::default(),
            PutOpts::default(),
        )
        .unwrap();
        let r = run(&s).unwrap();
        assert!(r.is_clean(), "{:?}", r);
        assert_eq!(r.managed, 1);
    }

    #[test]
    fn detects_stale_and_uncarded() {
        let root = tmp("check_stale");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        fs::write(root.join("src/b.rs"), "fn b(){}").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        s.put(
            &[entry("src/a.rs", "甲")],
            &Limits::default(),
            PutOpts::default(),
        )
        .unwrap();
        fs::write(root.join("src/a.rs"), "fn a(){2}").unwrap();
        let r = run(&s).unwrap();
        assert_eq!(r.stale, vec!["src/a.rs".to_string()]);
        assert_eq!(r.uncarded, 1);
        assert!(!r.complete);
    }

    #[test]
    fn reports_agents_block_without_dirtying_the_check() {
        let root = tmp("check_agents");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        s.put(
            &[entry("src/a.rs", "甲")],
            &Limits::default(),
            PutOpts::default(),
        )
        .unwrap();

        // 没有块：报 missing，但体检依然干净——缺块不该让老仓库突然变脏。
        let r = run(&s).unwrap();
        assert_eq!(r.agents_block, "missing");
        assert!(r.is_clean(), "{:?}", r);
        assert!(r.summary().contains("无 AGENTS.md 受管块"));

        // 写入块并给自己建卡：present，摘要不再提块。
        crate::agents::upsert(&root).unwrap();
        s.put(
            &[entry("AGENTS.md", "工作区声明")],
            &Limits::default(),
            PutOpts::default(),
        )
        .unwrap();
        let r = run(&s).unwrap();
        assert_eq!(r.agents_block, "present");
        assert!(r.is_clean(), "{:?}", r);
        assert!(!r.summary().contains("AGENTS.md 块"));

        // 手改回旧版正文：stale，提示可重写。
        let p = crate::agents::path(&root);
        let text = fs::read_to_string(&p)
            .unwrap()
            .replace("一行一文件", "一行一个文件");
        fs::write(&p, text).unwrap();
        let r = run(&s).unwrap();
        assert_eq!(r.agents_block, "stale");
        assert!(r.summary().contains("旧版"));
    }

    #[test]
    fn detects_tampered_line() {
        let root = tmp("check_tamper");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        s.put(
            &[entry("src/a.rs", "甲")],
            &Limits::default(),
            PutOpts::default(),
        )
        .unwrap();
        let p = s.index_file();
        let t = fs::read_to_string(&p).unwrap().replace("F:甲", "F:乙");
        fs::write(&p, t).unwrap();
        let r = run(&s).unwrap();
        assert_eq!(r.malformed.len(), 1);
        assert!(!r.is_clean());
    }
}
