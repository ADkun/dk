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
/// `matched.absent` 最多列几个词：够换词就够，长查询不该刷屏；总数另给 `absent_n`。
const ABSENT_LIST_MAX: usize = 8;

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
    let known = TOOLS
        .iter()
        .any(|t| t.strip_prefix("dk_").unwrap_or(t) == name);
    if !known {
        bail!("未知工具 {name}；可用：{}", TOOLS.join(" / "));
    }
    reject_unknown_args(name, args)?;
    match name {
        "init" => init(store, args),
        "map" => map(store, args),
        "get" => get(store, args),
        "put" => put(store, args),
        "sweep" => sweep_tool(store, args),
        "check" => check_tool(store),
        _ => unreachable!("TOOLS 与上面的分支必须一一对应"),
    }
}

/// 每个工具收哪些参数（与 mcp.rs 的 tool_specs、package/index.js 的 SPECS 同源）。
fn arg_keys(name: &str) -> &'static [&'static str] {
    match name {
        "init" => &["agents"],
        "map" => &[
            "task",
            "hints",
            "paths",
            "dir",
            "level",
            "k",
            "allow_partial",
        ],
        "get" => &["paths"],
        "put" => &["entries", "drop_bad"],
        "sweep" => &["limit"],
        _ => &[],
    }
}

/// 三处声明都写了 additionalProperties: false，实现不真拒就只是句场面话：
/// 拼错一个 hint（少个 s）会被静默丢掉，模型以为给了提示，检索里根本没有。
fn reject_unknown_args(name: &str, args: &Value) -> Result<()> {
    let Some(obj) = args.as_object() else {
        return Ok(());
    };
    let allowed = arg_keys(name);
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            let takes = if allowed.is_empty() {
                format!("dk_{name} 不收参数")
            } else {
                format!("dk_{name} 只收 {}", allowed.join(" / "))
            };
            bail!("未知参数 {key}：{takes}（拼错的参数会被静默忽略，所以这里直接报错）");
        }
    }
    Ok(())
}

/// 存在但不是字符串就报错，不当成「没给」：task 被传成数组时静默退成目录树，
/// 调用方拿回的是一句「要内容就给 task」——问了等于没问（审查 F2）。
fn opt_str(args: &Value, key: &str) -> Result<Option<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
        Some(other) => bail!("{key} 必须是字符串（收到 {other}）"),
    }
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
    // 先校验参数再动手：类型不对却在仓库里留下一个 .dk/，等于用报错换半个索引。
    // 受管块是索引的常驻入口，默认就写；只有显式传 false 才跳过。
    // 类型不符直接报错、不退回默认值："false" 这类字符串静默当 true 会让人以为关掉了。
    let want_block = match args.get("agents") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(other) => bail!("agents 必须是布尔值（收到 {other}）"),
    };
    let existed = store.is_initialized();
    store.ensure_layout()?;
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
    let level = opt_str(args, "level")?
        .unwrap_or_else(|| "l2".to_string())
        .to_lowercase();
    if !matches!(level.as_str(), "l0" | "l1" | "l2") {
        bail!("level 只能是 l0 / l1 / l2（收到 {level}）");
    }
    // 类型先严格判定：allow_partial 收到 "true" 会被当成 false，等于悄悄关掉调用方的意图。
    let allow_partial = match args.get("allow_partial") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(other) => bail!("allow_partial 必须是布尔值（收到 {other}）"),
    };
    let task_arg = opt_str(args, "task")?;
    let dir_arg = opt_str(args, "dir")?;
    let has_paths = args.get("paths").is_some_and(|v| !v.is_null());
    let has_dir = dir_arg.as_deref().is_some_and(|d| !d.is_empty());
    let has_task = task_arg.as_deref().is_some_and(|t| !t.is_empty());
    if args.get("task").is_some_and(|v| !v.is_null()) && !has_task {
        bail!("task 是空字符串：想给目录树就别传 task，想检索就给任务描述");
    }
    if has_paths && has_dir {
        bail!("paths 与 dir 只能给一个：paths 按文件取卡，dir 按目录切片");
    }
    if has_task && (has_paths || has_dir) {
        bail!("task 与 paths/dir 只能给一个：路径是直接取卡，不做检索");
    }
    // 四种模式互斥，任务检索专属参数也互斥：同时给、或没有 task 就给 hints/k/level/
    // allow_partial，工具都不能替调用方挑一个然后闭嘴（§3「静默退回默认值会让调用方
    // 以为自己问过了」）。审查实测：task+paths 同给时 task 被无声丢弃，只给 hints 时
    // 直接退成目录树，dir+level:"l0" 照样给全卡——三处都是「问了等于没问」。
    // 但显式传默认值不算「给了」：客户端常把 schema 里所有字段一次发齐，因此只有
    // level 不是 l2、allow_partial 为 true 才拒（审查 F1 的假阳性）。
    if !has_task {
        for key in ["hints", "k"] {
            let given = match args.get(key) {
                None | Some(Value::Null) => false,
                Some(Value::Array(a)) => !a.is_empty(),
                Some(Value::String(s)) => !s.trim().is_empty(),
                Some(_) => true,
            };
            if given {
                bail!("{key} 只属于任务检索：没有 task 它无从生效，别给");
            }
        }
        if level != "l2" {
            bail!("level 只属于任务检索：没有 task 它无从生效，别给");
        }
        if allow_partial {
            bail!("allow_partial 只属于任务检索：没有 task 它无从生效，别给");
        }
    }

    // 显式路径：不做检索，直接按路径取。
    if args.get("paths").is_some_and(|v| !v.is_null()) {
        let want: Vec<String> = arg_strs(args, "paths")?
            .iter()
            .map(|s| s.replace('\\', "/"))
            .collect();
        // 空数组 / 空字符串都给不出卡片，却会回一份「成功但空」的结果：说清楚比留白好。
        if want.is_empty() {
            bail!("paths 不能为空：给至少一个仓库内路径，或改用 task / dir");
        }
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
    if let Some(d) = dir_arg.as_deref().filter(|d| !d.is_empty()) {
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
    let Some(task) = task_arg.filter(|t| !t.is_empty()) else {
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
    if partial && !allow_partial {
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
    // k=0 时命中被裁空，confidence 与 matched 会跟着说「词面没有匹配」——那是假话。
    // 与其让判断依据被 k 带走，不如直接拒收这个值。
    if k == 0 {
        bail!("k 是返回张数，至少 1（收到 0）：想知道有没有命中看 confidence 与 matched");
    }
    let hits = idx.retrieve(&q, &task, k, SPREAD);
    let terms = Index::query_terms(&q);
    let span = idx.span(&terms);
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
    // 字面兜底的命中不是结论，单独一档。
    let degraded = hits.first().map(|h| h.why == "cjk").unwrap_or(false);
    // 三档只说「词是怎么对上的」，不承诺「答案在里面」：
    // 实测报 ok 的 142 条里 43 条（30%）正确答案不在前三，且没有任何阈值能把它们分开。
    let confidence = if top_score <= 0.0 {
        "none"
    } else if degraded {
        "fallback"
    } else {
        "surface"
    };
    // 只把有判别力的词算进分母：本仓到处都有的词算进去几乎必然命中，会让 of 天然虚高、
    // 报警饱和（实测报警 precision 只有 37%，一半多是假警报）。没有停用词表：中文虚词靠
    // 「出现在几乎每张卡里」落进 weak，英文虚词因为本仓根本没有这些词而落进 absent。
    let matched: Vec<&str> = hits
        .first()
        .map(|h| idx.hit_terms(h.idx, &span.strong))
        .unwrap_or_default();
    let absent: Vec<&str> = span.absent.iter().take(ABSENT_LIST_MAX).copied().collect();
    // 三档只说「词是怎么对上的」，并且把「你的词根本不在这个仓库里」单独讲出来——
    // 只报「词面没有匹配」，跨语言用户会以为是自己问法不好，而不是换一套词。
    let hint: String = if top_score <= 0.0 {
        // 三种「为什么没对上」必须分开说：本仓没有这些词 / 词太常见没判别力 /
        // 有判别力却一个也没命中。混成一句，小仓库里就会说出「你的词本仓一个也没有」这种谎话。
        let mut h = String::new();
        if !span.absent.is_empty() {
            let sample: Vec<&str> = span.absent.iter().take(3).copied().collect();
            h.push_str(&format!(
                "查询里 {} 个词在本仓卡片中一个也没出现（{}）",
                span.absent.len(),
                sample.join("、")
            ));
        }
        if !span.strong.is_empty() {
            if !h.is_empty() {
                h.push_str("；");
            }
            h.push_str(&format!("{} 个有判别力的查询词一个也没对上", span.strong.len()));
        }
        if h.is_empty() {
            // 走到这里有三种完全不同的原因，不能混成一句：词都太常见；分词后一个词都没有
            // （emoji、纯标点、孤字）；或者词全被当成跨词边界的二元组丢掉了（韩文、泰文这类
            // 无空格文字整句查英文仓库时就是这一种——说「没有能检索的词」等于怪用户乱输）。
            h.push_str(&if !span.weak.is_empty() {
                "查询词在本仓都太常见（过半卡片都有），没有判别力：换成更具体的词，或直接给 paths"
                    .to_string()
            } else if terms.is_empty() {
                // 走到这里的是分词后真一个词都没有的输入（emoji、纯标点、单字母）；
                // 单个汉字会留下一个词元，走下面的 else，别把两件事混成一句。
                "查询里没有能检索的词（emoji、纯标点、单字母都会这样）：换成文字描述，或直接给 paths"
                    .to_string()
            } else {
                let why = if terms.len() == 1 && terms[0].chars().count() == 1 {
                    "单个字不是索引里的词元"
                } else {
                    "无空格文字按相邻二元组切，没逐条列出"
                };
                format!(
                    "查询里 {} 个词在本仓卡片中一个也没出现（{why}）：换成源码/卡片里出现过的词，或直接给 paths",
                    terms.len()
                )
            });
        } else {
            h.push_str("。改用 paths / dir 指定范围，或先自己 rg 一轮再带路径来问");
        }
        h
    } else if degraded {
        "只按字面字符兜底对上的，别当成结论：给 paths / dir 缩小范围，或自己 rg 一轮再来".to_string()
    } else if span.strong.is_empty() {
        // 只剩「本仓到处都有」的词：排序确实动了，但一个判别力都没有，hit 必然为空。
        // 不说清楚，调用方会以为 matched 坏了，或者以为这几张就是答案。
        // l0 返回的是目录不是卡，措辞跟着换（审查 F3）。
        let unit = if level == "l0" { "这几个方向" } else { "这几张" };
        format!("查询词在本仓都有、但过半卡片都有（没有判别力）：hit 为空是正常的，{unit}只按常见词排了个序——换成更具体的词，或直接给 paths")
    } else {
        "matched 只数有判别力的词：of 是分母（本仓出现过、且不是过半卡片都有的词），hit 是首张卡踩中的，absent 是本仓没有的词。absent 多就换词（hints 放源码/报错里的标识符），命中少就给 paths。词面重叠不等于答案在里面：卡片是导航，源码仍是事实".to_string()
    };
    let mut body = json!({
        "mode": "task",
        "level": level,
        "query": q,
        "confidence": confidence,
        "matched": {
            "hit": matched,
            "of": span.strong.len(),
            "weak_n": span.weak.len(),
            "absent": absent,
            "absent_n": span.absent.len(),
            "total": terms.len(),
        },
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
