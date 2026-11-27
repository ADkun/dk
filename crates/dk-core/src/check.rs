//! 全量体检（`dk check`，也是 `dk_check` 工具）：唯一会哈希全部源码的地方。
//!
//! 这里慢一点没关系——它不进模型流程，是人的命令。所以不做增量、不维护账本，
//! 每次都是「遍历 + 并行哈希 + 逐卡比对」，结论只依赖磁盘现状。

use crate::card::Card;
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
    /// 行号 + 原因（含行尾校验和不符）+ 行摘要。
    pub malformed: Vec<String>,
    /// 可原地修复的坏行：正文能解析、且卡上的 @src_hash 与当前源码哈希一致——
    /// 只差 #hash 失配，重签一个正确的 #hash 即可（`dk check --fix` 干这个）。
    pub repairable: Vec<String>,
    /// 软门告警（SPEC §8）：写进去但不阻断，只在这里列出来让人看见。
    pub soft_warnings: Vec<String>,
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
            ("可修复", self.repairable.len()),
            ("软门", self.soft_warnings.len()),
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
    if !errors.is_empty() {
        let raw_lines = store.raw_lines()?;
        for (line, e) in &errors {
            let text = raw_lines
                .get(line.saturating_sub(1))
                .map(|s| s.trim_end())
                .unwrap_or("");
            rep.malformed
                .push(format!("第 {line} 行：{e}｜{}", preview_line(text)));
            if let Some(card) = repairable_card(text, root) {
                rep.repairable.push(format!(
                    "第 {line} 行：{}（保住 @{} 与字段，只重签 #hash）",
                    card.path, card.src_hash
                ));
            }
        }
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

    let limits = crate::store::Limits::default();
    for card in by_path.values() {
        rep.soft_warnings
            .extend(crate::store::soft_gate(card, &limits));
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

/// 坏行预览：给 check 报「这一行长什么样」，省得为了看懂一个 checksum mismatch 去翻整份索引。
fn preview_line(text: &str) -> String {
    let t = text.trim();
    let mut chars = t.chars();
    let head: String = chars.by_ref().take(72).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 一行坏行能否「只重签 #hash」修好：去掉行尾 #hash 后正文必须能解析成卡，
/// 且卡上的 @src_hash 必须等于当前源码哈希——否则源码已变（stale），得重读重写。
fn repairable_card(text: &str, root: &Path) -> Option<Card> {
    let sep = text.rfind(" #")?;
    let body = &text[..sep];
    let card = Card::parse_body(body).ok()?;
    let h = std::fs::read(root.join(&card.path))
        .ok()
        .map(|b| crate::card::short_hash(&b))?;
    (h == card.src_hash).then_some(card)
}

/// `dk check --fix` 的修复报告。
#[derive(Debug, Clone, Default, Serialize)]
pub struct RepairReport {
    pub repaired: usize,
    pub unrepaired: usize,
    pub details: Vec<String>,
}

/// `dk check --fix`：把「只差 #hash」的坏行按现行内容重签，其余原样保留并报告。
/// 调用方需已持有写锁（CLI 的 Check --fix 会先 Lock::acquire）。
pub fn repair(store: &Store) -> Result<RepairReport> {
    let path = store.index_file();
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut out_lines: Vec<String> = Vec::new();
    let mut rep = RepairReport::default();
    for (i, line) in raw.lines().enumerate() {
        let t = line.trim_end();
        if t.is_empty() || t.starts_with('#') {
            out_lines.push(line.to_string());
            continue;
        }
        match Card::parse(t) {
            Ok(_) => out_lines.push(line.to_string()),
            Err(e) => {
                if let Some(card) = repairable_card(t, store.root()) {
                    out_lines.push(card.render());
                    rep.repaired += 1;
                    rep.details
                        .push(format!("第 {} 行：已重签（{}）", i + 1, card.path));
                } else {
                    out_lines.push(line.to_string());
                    rep.unrepaired += 1;
                    rep.details.push(format!(
                        "第 {} 行：无法自动修复（{e}）——源码已变或正文不合法，请重读源码后 dk_put 重写",
                        i + 1
                    ));
                }
            }
        }
    }
    if rep.repaired > 0 {
        let mut text = out_lines.join("\n");
        text.push('\n');
        let tmp = path.with_extension("txt.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(rep)
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
            ..Default::default()
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

    /// 只改字段不碰 @src_hash 的坏行 → 可判为「可修复」，repair 就地重签，无需重读重写。
    #[test]
    fn repairable_bad_line_is_fixed_in_place() {
        let root = tmp("check_repair");
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
        assert_eq!(r.repairable.len(), 1, "正文合法且 @src_hash 未变，应可修复");
        let rr = repair(&s).unwrap();
        assert_eq!(rr.repaired, 1);
        assert_eq!(rr.unrepaired, 0);
        let r = run(&s).unwrap();
        assert!(r.malformed.is_empty(), "修复后不该再有坏行");
        assert!(r.is_clean(), "{:?}", r);
    }

    /// 源码已变的坏行 → @src_hash 对不上，不可修复，只能重读重写。
    #[test]
    fn stale_bad_line_is_not_repairable() {
        let root = tmp("check_stale_bad");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        s.put(
            &[entry("src/a.rs", "甲")],
            &Limits::default(),
            PutOpts::default(),
        )
        .unwrap();
        // 既手改字段、又让源码内容变了。
        let p = s.index_file();
        let t = fs::read_to_string(&p).unwrap().replace("F:甲", "F:乙");
        fs::write(&p, t).unwrap();
        fs::write(root.join("src/a.rs"), "fn a(){ /* changed */ }").unwrap();
        let r = run(&s).unwrap();
        assert_eq!(r.malformed.len(), 1);
        assert!(r.repairable.is_empty(), "源码已变，不该判可修复");
        let rr = repair(&s).unwrap();
        assert_eq!(rr.repaired, 0);
        assert_eq!(rr.unrepaired, 1);
    }

    /// 软门：超建议长度的卡写进去了，但 check 会告警；它不弄脏 is_clean。
    #[test]
    fn soft_warnings_surface_without_dirtying_check() {
        let root = tmp("check_soft");
        fs::write(root.join("src/a.rs"), "fn a(){}").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        let long = "x".repeat(201);
        s.put(
            &[entry("src/a.rs", &long)],
            &Limits::default(),
            PutOpts::default(),
        )
        .unwrap();
        let r = run(&s).unwrap();
        assert!(
            r.soft_warnings.iter().any(|w| w.contains("F 超过")),
            "应报软门告警，实际 {:?}",
            r.soft_warnings
        );
        assert!(r.is_clean(), "软门不该弄脏 is_clean：{:?}", r);
    }
}
