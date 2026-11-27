//! dk 命令行。人用的部分（init / check）与模型用的部分（其余）共用同一套核心。

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use dk_core::check;
use dk_core::serve;
use dk_core::store::{Lock, PutEntry, Store};
use dk_core::sweep;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "dk", version, about = "轻量代码索引：边际成本只有一次 dk_put")]
struct Cli {
    /// 仓库根目录
    #[arg(short = 'C', long, global = true, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 建立 .dk/ 与 .dkignore，并写入 AGENTS.md 受管块（人触发，不写卡）
    Init {
        /// 不写 AGENTS.md 受管块
        #[arg(long)]
        no_agents: bool,
    },
    /// 还差哪些文件没建卡
    Sweep {
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// 全量体检：过期 / 失联 / 未建卡 / 坏行
    Check {
        #[arg(long)]
        json: bool,
        /// 就地修复「只差 #hash」的坏行（正文合法且 @src_hash 仍对得上源码）
        #[arg(long)]
        fix: bool,
    },
    /// 取全卡 + 同目录兄弟 + 新鲜度
    Get {
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// 写卡：从 JSON 文件或 stdin 读 {entries:[...]}
    Put {
        /// JSON 文件路径；省略则读 stdin
        file: Option<PathBuf>,
        /// 清掉索引里无法解析的历史坏行（默认保留，供人查看）
        #[arg(long)]
        drop_bad: bool,
        /// 只算不写：打印将要 added / updated / removed / dropped 的结果，不落盘
        #[arg(long)]
        dry_run: bool,
    },
    /// 按任务检索；无参时给目录树
    Map {
        task: Vec<String>,
        /// 索引不全时仍然给结果（默认拒绝）
        #[arg(long)]
        allow_partial: bool,
        /// 披露级别：l0 目录 / l1 清单 / l2 全卡（默认 l2）
        #[arg(long)]
        level: Option<String>,
    },
    /// NDJSON 服务：每行 {id,tool,args} -> {id,ok,result|error}
    Serve,
    /// MCP stdio 服务器（JSON-RPC 2.0），供非 DSH 的 harness 接入
    Mcp,
    /// 统计卡片体量与 token 估算
    Tokens,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("dk: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let store = Store::open(&cli.root)?;
    match cli.cmd {
        Cmd::Init { no_agents } => {
            let v = serve::handle(&store, "dk_init", &json!({ "agents": !no_agents }))?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        Cmd::Sweep { limit } => {
            // 没有 .dk 时「还差 0 个」是假绿：同一目录下 dk_map 会说「这里没有索引」，
            // 两个壳对同一个事实给两套答案，其中一套会让人以为检查过了（审查 P1-2）。
            serve::require_repo(&store)?;
            let b = sweep::sweep(&store, limit)?;
            println!(
                "受管 {} 文件｜已建卡 {}｜还差 {}",
                b.total, b.done, b.remaining
            );
            for it in &b.batch {
                let flags = if it.flags.is_empty() {
                    "-".to_string()
                } else {
                    it.flags.join(",")
                };
                println!(
                    "  {:<60} {:>8} {:>6}行  {}",
                    it.path,
                    human(it.bytes),
                    it.lines,
                    flags
                );
            }
            if b.remaining > 0 {
                println!("（重跑即续跑；模型建卡用 dk_put）");
            }
            Ok(())
        }
        Cmd::Check { json: as_json, fix } => {
            serve::require_repo(&store)?;
            if fix {
                store.ensure_layout()?;
                let _lock = Lock::acquire(store.dir(), Duration::from_secs(30))?;
                let rr = check::repair(&store)?;
                println!("修复 {} 行，未修复 {} 行", rr.repaired, rr.unrepaired);
                for d in rr.details.iter().take(50) {
                    println!("  {d}");
                }
            }
            let r = check::run(&store)?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("{}", r.summary());
                for (label, list) in [
                    ("过期（源码已变）", &r.stale),
                    ("失联（卡还在文件没了）", &r.missing),
                    ("坏 R", &r.bad_r),
                    ("坏行", &r.malformed),
                    ("可修复坏行（dk check --fix）", &r.repairable),
                    ("重复卡", &r.duplicate),
                ] {
                    if list.is_empty() {
                        continue;
                    }
                    println!("\n{label}:");
                    for x in list.iter().take(50) {
                        println!("  {x}");
                    }
                }
                if r.uncarded > 0 {
                    println!(
                        "\n未建卡 {} 个，前 {}：",
                        r.uncarded,
                        r.uncarded_sample.len()
                    );
                    for x in &r.uncarded_sample {
                        println!("  {x}");
                    }
                }
                if !r.soft_warnings.is_empty() {
                    println!("\n软门告警（已写入，只是超建议长度，不阻断）：");
                    for x in r.soft_warnings.iter().take(50) {
                        println!("  {x}");
                    }
                }
            }
            if !r.is_clean() {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Get { paths } => {
            let v = serve::handle(&store, "dk_get", &json!({ "paths": paths }))?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        Cmd::Put {
            file,
            drop_bad,
            dry_run,
        } => {
            let raw = match file {
                Some(p) => {
                    std::fs::read_to_string(&p).with_context(|| format!("读 {}", p.display()))?
                }
                None => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            let args: Value = serde_json::from_str(&raw).context("不是合法 JSON")?;
            // CLI 侧自己拿锁：serve 的 handle 也拿一次会冲突，这里直接走 store.put。
            let entries: Vec<PutEntry> = serde_json::from_value(
                args.get("entries").cloned().unwrap_or(Value::Array(vec![])),
            )
            .context("entries 格式错误")?;
            if entries.is_empty() {
                bail!("entries 不能为空");
            }
            // 先建目录再拿锁，否则没 init 的仓库只会得到一句「系统找不到指定的路径」。
            store.ensure_layout()?;
            let _lock = Lock::acquire(store.dir(), Duration::from_secs(30))?;
            let opts = dk_core::store::PutOpts { drop_bad, dry_run };
            let rep = store.put(&entries, &dk_core::store::Limits::default(), opts)?;
            println!("{}", serde_json::to_string_pretty(&rep)?);
            if dry_run {
                eprintln!("dry_run：以上仅为预览，未写入索引（去掉 --dry-run 才落盘）");
            }
            Ok(())
        }
        Cmd::Map {
            task,
            allow_partial,
            level,
        } => {
            let text = task.join(" ");
            // 只转发人真给过的参数：clap 的默认值一旦无条件塞进 args，「无参给目录树」
            // 就会被 serve 的互斥守卫当成「没有 task 却给了 level」当场打死（审查 F1）。
            let mut args = json!({});
            if !text.trim().is_empty() {
                args["task"] = json!(text);
            }
            if let Some(l) = level {
                args["level"] = json!(l);
            }
            if allow_partial {
                args["allow_partial"] = json!(true);
            }
            let v = serve::handle(&store, "dk_map", &args)?;
            print_map(&v);
            Ok(())
        }
        Cmd::Serve => serve_loop(&store),
        Cmd::Mcp => dk_core::mcp::run(&store),
        Cmd::Tokens => tokens(&store),
    }
}

fn print_map(v: &Value) {
    if let Some(arr) = v.get("dirs").and_then(|x| x.as_array()) {
        // tree 模式的 dir 已经按「目录带尾斜杠、顶层文件不带」给定，再补一个就成了
        // crates// 与 AGENTS.md/；L0 检索给的是不带尾斜杠的父目录，补一个才清楚。
        let tree = v.get("mode").and_then(|x| x.as_str()) == Some("tree");
        if tree {
            println!("（只给目录形状：给任务描述则按任务检索并出卡片；--level 只对检索生效）");
        } else {
            println!("（L0：只给方向，要内容去掉 --level）");
        }
        for d in arr {
            let dir = d.get("dir").and_then(|x| x.as_str()).unwrap_or("");
            let n = d
                .get("hits")
                .or_else(|| d.get("cards"))
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let shown = if tree {
                dir.to_string()
            } else {
                format!("{}/", dir.trim_end_matches('/'))
            };
            println!("  {shown}  {n}");
        }
        if !tree {
            print_matched(v, Shape::Dirs);
            // L0 也要给 hint：服务端对 l0 是带 hint 的，CLI 不印就等于把「为什么这几个
            // 目录」的理由吞了，只剩一张无从核对的清单（审查 M5）。
            if let Some(h) = v.get("hint").and_then(|x| x.as_str()) {
                eprintln!("\n{h}");
            }
        }
        return;
    }
    if let Some(arr) = v.get("cards").and_then(|x| x.as_array()) {
        if v.get("status").is_some() {
            println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
            return;
        }
        for c in arr {
            match c.as_str() {
                Some(s) => println!("{s}"),
                None => println!("{c}"),
            }
        }
        // 命中行优先：没有卡片时 matched 里那行往往就是唯一有用的提示。
        if !print_matched(v, Shape::Cards) && arr.is_empty() {
            println!("（无匹配。改用目录或路径指定范围，或先自己搜一轮再带路径来问）");
        }
        if let Some(h) = v.get("hint").and_then(|x| x.as_str()) {
            eprintln!("\n{h}");
        }
        return;
    }
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// 只报「首张卡命中了哪些有判别力的词」，不报分数：分数不可比，词才是人能核对的证据。
/// 三种任务档（卡片 / L0 目录）都要印——不印出来，hint 里那句「命中的少就换词」就没依据。
/// 本仓一个词都没有时也要印：那正是「你该换一套词」的唯一提示。
/// 返回是否印了东西：没有卡片时这行往往就是唯一的提示，不能和「无匹配」那句二选一。
/// 输出形态：同样的 matched，在卡片清单下说「几张」，在目录清单下必须说「几个目录」，
/// 否则会指着目录讲卡片（审查 F3）。
#[derive(Clone, Copy)]
enum Shape {
    Cards,
    Dirs,
}

fn print_matched(v: &Value, shape: Shape) -> bool {
    let Some(m) = v.get("matched") else {
        return false;
    };
    let of = m.get("of").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let absent_n = m.get("absent_n").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let weak_n = m.get("weak_n").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let total = m.get("total").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let list = |k: &str| -> Vec<&str> {
        m.get(k)
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default()
    };
    if of == 0 {
        // 没有有判别力的词时，三种原因要分开说：太常见 / 本仓没有 / 压根没分出词
        // （emoji、纯标点，以及被当成跨词二元组丢弃的无空格文字）。
        let mut parts: Vec<String> = Vec::new();
        if weak_n > 0 {
            parts.push(format!("{weak_n} 个词本仓都有但过半卡片都有、没有判别力"));
        }
        if absent_n > 0 {
            parts.push(format!(
                "{absent_n} 个词本仓一张卡都没有：{}",
                list("absent").join("、")
            ));
        }
        if parts.is_empty() {
            if total == 0 {
                return false;
            }
            // 词都在本仓：要么是被当成跨词边界丢掉的无空格二元组，要么压根不是索引里的
            // 词元（单字最典型——卡片正文有它，索引里没有这一个词）。这两种若说成
            // 「本仓一张卡都没有」，就是当着卡片的面说没有卡片（审查 S1）。
            let fb = v.get("confidence").and_then(|x| x.as_str()) == Some("fallback");
            let shown = match shape {
                Shape::Cards => "下面几张",
                Shape::Dirs => "这几个目录",
            };
            parts.push(if fb {
                format!("{total} 个词不是索引里的词元（单字、极短串都这样），{shown}是按字面字符兜底对上的")
            } else if total == 1 {
                // 「无空格文字按相邻二元组切」对一个字说不通：没有相邻可言（审查 F4）。
                "这 1 个词不是索引里的词元（单个字或极短串）：换成源码/卡片里出现过的词".to_string()
            } else {
                format!("{total} 个词在本仓卡片里都没有（无空格文字按相邻二元组切）")
            });
        }
        println!("（{}）", parts.join("；"));
        return true;
    }
    let hit = list("hit");
    let mut parts: Vec<String> = Vec::new();
    if hit.is_empty() {
        parts.push(format!("{of} 个有判别力的查询词一个也没命中"));
    } else {
        parts.push(format!(
            "首张卡命中 {}/{} 个有判别力的查询词：{}",
            hit.len(),
            of,
            hit.join("、")
        ));
    }
    if absent_n > 0 {
        parts.push(format!(
            "另有 {absent_n} 个词本仓没有：{}",
            list("absent").join("、")
        ));
    }
    println!("（{}）", parts.join("；"));
    true
}

fn serve_loop(store: &Store) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        // 非 UTF-8 字节不能让整个会话死掉：一轮乱码就断掉 MCP 通道，
        // 调用方只看到「工具没有返回」，比报错难查一百倍。回一帧、接着读下一行。
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                writeln!(
                    out,
                    "{}",
                    json!({"id": null, "ok": false, "error": format!("坏请求：这一行不是合法 UTF-8（{e}）")})
                )?;
                out.flush()?;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                writeln!(
                    out,
                    "{}",
                    json!({"id": null, "ok": false, "error": format!("坏请求：{e}")})
                )?;
                out.flush()?;
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let tool = req
            .get("tool")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let args = req.get("args").cloned().unwrap_or(json!({}));
        let resp = match serve::handle(store, &tool, &args) {
            Ok(v) => json!({"id": id, "ok": true, "result": v}),
            Err(e) => json!({"id": id, "ok": false, "error": format!("{e:#}")}),
        };
        writeln!(out, "{resp}")?;
        out.flush()?;
    }
    Ok(())
}

fn tokens(store: &Store) -> Result<()> {
    let raw = std::fs::read_to_string(store.index_file()).unwrap_or_default();
    let (cards, errs) = store.load()?;
    let bytes = raw.len();
    let lens: Vec<usize> = cards.iter().map(|c| c.render().len()).collect();
    let avg = if lens.is_empty() {
        0
    } else {
        lens.iter().sum::<usize>() / lens.len()
    };
    let full = lens.iter().sum::<usize>();
    println!(
        "卡片 {} 张｜坏行 {}｜map.txt {} B｜估算 {} token",
        cards.len(),
        errs.len(),
        bytes,
        bytes / 3
    );
    println!(
        "单卡平均 {} B（≈{} token）｜全量 {} B（≈{} token）",
        avg,
        avg / 3,
        full,
        full / 3
    );
    println!(
        "L1+L2 默认披露（6 张卡 + 5 邻居）≈ {} token",
        (avg * 6 + 5 * 70) / 3
    );
    Ok(())
}

fn human(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1}MB", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1}KB", n as f64 / (1u64 << 10) as f64)
    } else {
        format!("{n}B")
    }
}
