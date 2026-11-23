//! 工具分发：六个工具的唯一定义处（SPEC §3）。
//!
//! 只做纯函数式的一次调用——没有会话状态、没有待办队列、没有前置阶段。
//! `dk serve` 的 NDJSON 循环与 MCP 壳都调 `handle`，不各写一份。

use crate::agents;
use crate::card::Card;
use crate::check;
use crate::search::{parent_dir, Index};
use crate::store::{Limits, PutEntry, Store};
use crate::sweep;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// 检索默认返回张数（SPEC §5：L1+L2 混合 5–8 张）。
const TOP_K: usize = 6;
/// 扩散张数上限（R 邻居只给一行摘要，不占卡片预算）。
const SPREAD: usize = 5;

pub const TOOLS: [&str; 6] = [
    "dk_init", "dk_map", "dk_get", "dk_put", "dk_sweep", "dk_check",
];

pub fn handle(store: &Store, tool: &str, args: &Value) -> Result<Value> {
    // 目录不存在时不说「没有索引」：那会让人照着 hint 在错的地方再建一份（审查 S2/G5）。
    if !store.root().is_dir() {
        bail!(
            "工作目录不存在：{}（-C / root 要指向真实存在的目录）",
            store.root().display()
        );
    }
    let name = tool.strip_prefix("dk_").unwrap_or(tool);
    match name {
        "init" => init(store, args),
        "map" => map(store, args),
        "get" => get(store, args),
        "put" => put(store, args),
        "sweep" => sweep_tool(store, args),
        "check" => check_tool(store),
        other => bail!("未知工具 {other}；可用：{}", TOOLS.join(" / ")),
    }
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 一个参数既接受字符串也接受字符串数组——声明的类型与实现不一致时，
/// 静默丢掉调用方给的提示，比报错更伤人。
fn arg_strs(args: &Value, key: &str) -> Result<Vec<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => {
            let s = s.trim();
            Ok(if s.is_empty() {
                Vec::new()
            } else {
                vec![s.to_string()]
            })
        }
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for v in a {
                match v.as_str() {
                    Some(s) => {
                        let s = s.trim();
                        if !s.is_empty() {
                            out.push(s.to_string());
                        }
                    }
                    None => bail!("{key} 必须是字符串数组（收到元素 {v}）"),
                }
            }
            Ok(out)
        }
        Some(other) => bail!("{key} 必须是字符串或字符串数组（收到 {other}）"),
    }
}

/// 类型不对就报错，不静默退回默认值：limit 传字符串被当成「没给」比报错更伤人（审查 G1）。
fn arg_usize(args: &Value, key: &str, dflt: usize) -> Result<usize> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(dflt),
        Some(v) => match v.as_u64() {
            Some(n) => Ok(n as usize),
            None => bail!("{key} 必须是非负整数（收到 {v}）"),
        },
    }
}

fn init(store: &Store, args: &Value) -> Result<Value> {
    let existed = store.is_initialized();
    store.ensure_layout()?;
    // 受管块是索引的常驻入口，默认就写；只有显式传 false 才跳过。
    // 类型不符直接报错、不退回默认值："false" 这类字符串静默当 true 会让人以为关掉了。
    let want_block = match args.get("agents") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(other) => bail!("agents 必须是布尔值（收到 {other}）"),
    };
    let block = if want_block {
        match agents::upsert(store.root()) {
            Ok(o) => o.as_str(),
            Err(e) => return Err(e.context(
                ".dk/ 与 .dkignore 已建；AGENTS.md 受管块没写（索引本身还没建，要靠 dk_put 建卡）",
            )),
        }
    } else {
        "skipped"
    };
    Ok(json!({
        "status": if existed { "exists" } else { "created" },
        "root": store.root().display().to_string(),
        "index": store.index_file().display().to_string(),
        "ignore": store.ignore_path().display().to_string(),
        "agents": { "file": agents::FILE, "status": block },
        "next": [
            "人：确认 .dkignore 排除范围（生成物、第三方、理论原文目录等）",
            "模型：dk_sweep 取批 → 读源码 → dk_put 写卡，循环到 remaining 为 0（AGENTS.md 也要一张卡，或 skip）",
        ],
    }))
}

fn map(store: &Store, args: &Value) -> Result<Value> {
    if !store.is_initialized() {
        return Ok(
            json!({"status": "uninitialized", "hint": "此仓库还没有 dk 索引，请人执行 dk init"}),
        );
    }
    let (cards, _) = store.load()?;
    let idx = Index::build(&cards);
    let level = arg_str(args, "level")
        .unwrap_or_else(|| "l2".to_string())
        .to_lowercase();
    if !matches!(level.as_str(), "l0" | "l1" | "l2") {
        bail!("level 只能是 l0 / l1 / l2（收到 {level}）");
    }

    // 显式路径：不做检索，直接按路径取。
    if args.get("paths").is_some_and(|v| !v.is_null()) {
        let want: Vec<String> = arg_strs(args, "paths")?
            .iter()
            .map(|s| s.replace('\\', "/"))
            .collect();
        let mut lines = Vec::new();
        let mut unknown = Vec::new();
        for w in want {
            match cards.iter().find(|c| c.path == w) {
                Some(c) => lines.push(c.render()),
                None => unknown.push(w),
            }
        }
        return Ok(json!({"mode": "paths", "cards": lines, "uncarded": unknown}));
    }

    // 目录切片：把某目录下所有卡原样给出（L1）。
    if let Some(d) = arg_str(args, "dir") {
        let d = d.trim_end_matches('/').replace('\\', "/");
        let mut lines: Vec<String> = cards
            .iter()
            .filter(|c| parent_dir(&c.path) == d || c.path.starts_with(&format!("{d}/")))
            .map(|c| c.render())
            .collect();
        lines.sort();
        return Ok(json!({"mode": "dir", "dir": d, "count": lines.len(), "cards": lines}));
    }

    // 无参：L0 目录树，只给形状不给内容。
    let Some(task) = arg_str(args, "task") else {
        let mut dirs: BTreeMap<String, usize> = BTreeMap::new();
        for c in &cards {
            let top = c.path.split('/').next().unwrap_or(&c.path).to_string();
            let top = if c.path.contains('/') {
                format!("{top}/")
            } else {
                top
            };
            *dirs.entry(top).or_insert(0) += 1;
        }
        let tree: Vec<Value> = dirs
            .iter()
            .map(|(d, n)| json!({"dir": d, "cards": n}))
            .collect();
        return Ok(json!({
            "mode": "tree",
            "cards": cards.len(),
            "dirs": tree,
            "hint": "要内容就给 task（自然语言任务描述），或 paths / dir",
        }));
    };

    // 检索：先看覆盖率，不完整就不给排序结果（部分索引比没有更危险）。
    // 但拒绝必须可操作：告诉人差多少个、差的是哪些，而不是一句「不完整」。
    let cov = coverage(store)?;
    let partial = cov.uncarded > 0;
    if partial
        && !args
            .get("allow_partial")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        return Ok(json!({
            "status": "incomplete",
            "managed": cov.managed,
            "uncarded": cov.uncarded,
            "uncarded_sample": cov.sample,
            "next": "先 dk_sweep 取批补卡；确认这些与本次任务无关，可传 allow_partial:true",
        }));
    }

    let mut q = task.clone();
    for h in arg_strs(args, "hints")? {
        q.push(' ');
        q.push_str(&h);
    }
    let k = arg_usize(args, "k", TOP_K)?;
    let hits = idx.retrieve(&q, &task, k, SPREAD);
    // 按披露级别取舍：L0 只给方向，L1 给清单，L2 给全卡（默认）。
    let mut dirs: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Vec::new();
    let mut seen = BTreeMap::new();
    for h in &hits {
        let c = &cards[h.idx];
        if seen.insert(c.path.clone(), ()).is_some() {
            continue;
        }
        match level.as_str() {
            "l0" => {
                let d = parent_dir(&c.path);
                let d = if d.is_empty() {
                    ".".to_string()
                } else {
                    d.to_string()
                };
                *dirs.entry(d).or_insert(0) += 1;
            }
            "l1" => out.push(format!("{} — {}", c.path, c.f)),
            _ => match h.why {
                "bm25" => out.push(c.render()),
                // 邻居只给一行摘要：省字节，且提示「顺着看哪」而不是「这就是答案」。
                _ => out.push(format!("{} — {}", c.path, c.f)),
            },
        }
    }
    let top_score = hits.first().map(|h| h.score).unwrap_or(0.0);
    // 字面兜底的命中一律按「低置信」上报：它是止损，不是结论。
    let degraded = hits.first().map(|h| h.why == "cjk").unwrap_or(false);
    let confidence = if top_score <= 0.0 {
        "none"
    } else if degraded || (out.is_empty() && dirs.is_empty()) {
        "low"
    } else {
        "ok"
    };
    let hint = if top_score <= 0.0 {
        "词面没有匹配。改用 paths / dir 指定范围，或先自己 rg 一轮再带路径来问"
    } else if degraded {
        "只按字面字符兜底对上的，别当成结论：给 paths / dir 缩小范围，或自己 rg 一轮再来"
    } else {
        "卡片是导航，源码仍是事实；要全卡用 dk_get，要源码自己读文件"
    };
    let mut body = json!({
        "mode": "task",
        "level": level,
        "query": q,
        "confidence": confidence,
        "coverage": cov.json(),
        "hint": hint,
    });
    if level == "l0" {
        let dirs: Vec<Value> = dirs
            .iter()
            .map(|(d, n)| json!({"dir": d, "hits": n}))
            .collect();
        body["dirs"] = json!(dirs);
    } else {
        body["count"] = json!(out.len());
        body["cards"] = json!(out);
        if partial {
            body["partial"] = json!(format!("本结果不含 {}", cov.sample.join("、")));
        }
    }
    Ok(body)
}

struct Coverage {
    managed: usize,
    uncarded: usize,
    /// 未建卡的样例路径：拒绝服务时给得出「差的是哪些」，人才能判断要不要管。
    sample: Vec<String>,
}

impl Coverage {
    fn json(&self) -> Value {
        let carded = self.managed.saturating_sub(self.uncarded);
        let pct = if self.managed == 0 {
            100.0
        } else {
            carded as f64 * 100.0 / self.managed as f64
        };
        json!({
            "managed": self.managed,
            "carded": carded,
            "uncarded": self.uncarded,
            "pct": (pct * 10.0).round() / 10.0,
        })
    }
}

/// 覆盖率 + 未建卡样例。只走目录树与已建卡集合，不哈希（10 万文件约 0.6 s）。
fn coverage(store: &Store) -> Result<Coverage> {
    let rules = crate::ignore_rules::Rules::load(store.root())?;
    let all = sweep::walk(store.root(), &rules)?.files;
    let (cards, _) = store.load()?;
    let carded: std::collections::BTreeSet<String> = cards.into_iter().map(|c| c.path).collect();
    let mut sample = Vec::new();
    let mut uncarded = 0usize;
    for p in &all {
        if !carded.contains(p) {
            uncarded += 1;
            if sample.len() < 5 {
                sample.push(p.clone());
            }
        }
    }
    Ok(Coverage {
        managed: all.len(),
        uncarded,
        sample,
    })
}

fn freshness(store: &Store, c: &Card) -> &'static str {
    match store.source_hash(&c.path) {
        None => "missing",
        Some(h) if h == c.src_hash => "fresh",
        _ => "stale",
    }
}

fn get(store: &Store, args: &Value) -> Result<Value> {
    if !store.is_initialized() {
        return Ok(
            json!({"status": "uninitialized", "hint": "此仓库还没有 dk 索引，请人执行 dk init"}),
        );
    }
    let want = arg_strs(args, "paths")?;
    if want.is_empty() {
        bail!("需要 paths：仓库相对路径的字符串数组，例如 [\"src/store.rs\"]");
    }
    let (cards, _) = store.load()?;
    let idx = Index::build(&cards);
    let mut out = Vec::new();
    for w in want {
        let w = w.replace('\\', "/");
        match cards.iter().position(|c| c.path == w) {
            None => out.push(
                json!({"path": w, "state": "uncarded", "hint": "无卡。读源码后 dk_put 补一张"}),
            ),
            Some(i) => {
                let c = &cards[i];
                let mut sib: Vec<Value> = idx
                    .siblings(i)
                    .into_iter()
                    .take(SPREAD)
                    .map(|j| json!({"path": cards[j].path, "F": cards[j].f}))
                    .collect();
                sib.sort_by_key(|v| v["path"].as_str().unwrap_or("").to_string());
                out.push(json!({
                    "path": c.path,
                    "state": freshness(store, c),
                    "card": c.render(),
                    // 同目录兄弟：命中一个文件后，这一族往往要一起动。
                    "siblings": sib,
                }));
            }
        }
    }
    Ok(json!({"count": out.len(), "cards": out}))
}

fn put(store: &Store, args: &Value) -> Result<Value> {
    let raw = args
        .get("entries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("需要 entries: [{{path, F?, R?, A?, S?, tags?, skip?}}]"))?;
    let entries: Vec<PutEntry> = serde_json::from_value(Value::Array(raw.clone()))
        .map_err(|e| anyhow::anyhow!("entries 格式错误：{e}"))?;
    // 先建目录再拿锁：否则没 init 的仓库会得到「create .dk\lock: 系统找不到指定的路径」。
    store.ensure_layout()?;
    let _lock = crate::store::Lock::acquire(store.dir(), std::time::Duration::from_secs(30))?;
    let opts = crate::store::PutOpts {
        drop_bad: args
            .get("drop_bad")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    let rep = store.put(&entries, &Limits::default(), opts)?;
    let cov = coverage(store)?;
    Ok(json!({
        "added": rep.added,
        "updated": rep.updated,
        "removed": rep.removed,
        "proposed_ignore": rep.proposed_ignore,
        "rejected": rep.rejected,
        "bad_lines_kept": rep.bad_lines_kept,
        "bad_lines_dropped": rep.bad_lines_dropped,
        "warnings": rep.warnings,
        "coverage": cov.json(),
        "next": if cov.uncarded > 0 { "dk_sweep 继续补齐" } else { "覆盖完整" },
    }))
}

fn sweep_tool(store: &Store, args: &Value) -> Result<Value> {
    if !store.is_initialized() {
        return Ok(
            json!({"status": "uninitialized", "hint": "此仓库还没有 dk 索引，请人执行 dk init"}),
        );
    }
    let limit = arg_usize(args, "limit", Limits::default().sweep_batch)?;
    let b = sweep::sweep(store, limit)?;
    Ok(json!({
        "total": b.total,
        "done": b.done,
        "remaining": b.remaining,
        // 不带源码：读文件是模型自己该做的动作，工具只回答「轮到谁」。
        "batch": b.batch,
        "hint": "flags 只是机器特征（minified/vendor/generated/lockfile/large/binary），值不值得建卡由你判断；不值得就 dk_put {path, skip:true}",
    }))
}

fn check_tool(store: &Store) -> Result<Value> {
    if !store.is_initialized() {
        return Ok(
            json!({"status": "uninitialized", "hint": "此仓库还没有 dk 索引，请人执行 dk init"}),
        );
    }
    let r = check::run(store)?;
    Ok(json!({
        "managed": r.managed,
        "cards": r.cards,
        "complete": r.complete,
        "stale": r.stale,
        "missing": r.missing,
        "uncarded": r.uncarded,
        "uncarded_sample": r.uncarded_sample,
        "bad_r": r.bad_r,
        "malformed": r.malformed,
        "duplicate": r.duplicate,
        "agents_block": r.agents_block,
        "summary": r.summary(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn tmp(tag: &str) -> PathBuf {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(tag);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn init_writes_the_block_and_can_skip_it() {
        let root = tmp("serve_init_agents");
        let store = Store::open(&root).unwrap();

        let v = handle(&store, "dk_init", &json!({ "agents": false })).unwrap();
        assert_eq!(v["agents"]["status"], "skipped");
        assert!(!root.join(agents::FILE).exists());

        let v = handle(&store, "dk_init", &json!({})).unwrap();
        assert_eq!(v["agents"]["status"], "created");

        let v = handle(&store, "dk_init", &json!({ "agents": true })).unwrap();
        assert_eq!(v["agents"]["status"], "unchanged");

        // 字符串 "false" 不许被当成 true 静默放过。
        assert!(handle(&store, "dk_init", &json!({ "agents": "false" })).is_err());
    }

    #[test]
    fn init_refuses_a_broken_block() {
        let root = tmp("serve_init_broken");
        let store = Store::open(&root).unwrap();
        fs::write(root.join(agents::FILE), "<!-- dk:begin -->\n用户正文\n").unwrap();
        // anyhow 的 Display 只给最外层 context，{:#} 才是完整链。
        let err = format!("{:#}", handle(&store, "dk_init", &json!({})).unwrap_err());
        assert!(err.contains("不成对"), "{err}");
    }
}
