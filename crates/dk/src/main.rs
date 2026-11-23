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
    },
    /// 按任务检索；无参时给目录树
    Map {
        task: Vec<String>,
        /// 索引不全时仍然给结果（默认拒绝）
        #[arg(long)]
        allow_partial: bool,
        /// 披露级别：l0 目录 / l1 清单 / l2 全卡
        #[arg(long, default_value = "l2")]
        level: String,
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
        Cmd::Check { json: as_json } => {
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
        Cmd::Put { file, drop_bad } => {
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
            let opts = dk_core::store::PutOpts { drop_bad };
            let rep = store.put(&entries, &dk_core::store::Limits::default(), opts)?;
            println!("{}", serde_json::to_string_pretty(&rep)?);
            Ok(())
        }
        Cmd::Map {
            task,
            allow_partial,
            level,
        } => {
            let text = task.join(" ");
            let args = json!({ "task": text, "allow_partial": allow_partial, "level": level });
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
        if arr.is_empty() {
            println!("（无匹配。改用目录或路径指定范围，或先自己搜一轮再带路径来问）");
        }
        if let Some(h) = v.get("hint").and_then(|x| x.as_str()) {
            eprintln!("\n{h}");
        }
        return;
    }
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn serve_loop(store: &Store) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
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
