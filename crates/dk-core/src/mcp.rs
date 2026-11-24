//! MCP 壳：stdio JSON-RPC 2.0 ↔ \`serve::handle\`（SPEC §9.2）。
//!
//! 只做协议翻译，不做业务判断。业务唯一入口是 \`serve::handle\`——\`dk serve\` 的
//! NDJSON 循环与这里的 tools/call 走的是同一份实现，不存在第二套逻辑。
//!
//! 传输：一行一个 JSON-RPC 消息（MCP stdio transport）。没有 Content-Length 头。

use crate::serve;
use crate::store::Store;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// 服务端认识的协议版本。客户端要求的版本在表里就照它回，否则退回最新的。
const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub fn run(store: &Store) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(line) {
            // 批量帧：MCP 不用 batch，但老客户端会发。静默丢弃会让它永久挂住（审查 G4）。
            Ok(req) if req.is_array() => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": {"code": -32600, "message": "不支持批量帧（batch）：一行一个请求对象"},
            })),
            Ok(req) => answer(store, &req),
            Err(e) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": {"code": -32700, "message": format!("请求不是合法 JSON：{e}")},
            })),
        };
        if let Some(resp) = resp {
            writeln!(out, "{resp}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// 一请求一响应。返回 None = 通知，按 JSON-RPC 2.0 不回包。
fn answer(store: &Store, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned()?;
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let resp = match method {
        "initialize" => Some(json!({
            "protocolVersion": negotiate(req),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "dk", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "本仓库有一份代码索引。改代码前先 dk_map 或 dk_get 看卡片，改完用 dk_put 写回改过的文件。卡片是导航，源码仍是事实。",
        })),
        "ping" => Some(json!({})),
        "tools/list" => Some(json!({"tools": tool_specs()})),
        "tools/call" => Some(call(store, req.get("params").cloned().unwrap_or(json!({})))),
        other => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("未知方法 {other}；本服务只提供 initialize / tools/list / tools/call / ping")},
            }))
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": resp.unwrap_or(Value::Null)}))
}

fn negotiate(req: &Value) -> String {
    let asked = req
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str());
    match asked {
        Some(v) if PROTOCOLS.contains(&v) => v.to_string(),
        _ => PROTOCOLS[0].to_string(),
    }
}

/// 工具错误按 MCP 惯例走 isError，而不是 JSON-RPC error——模型能看见并自己改参数。
fn call(store: &Store, params: Value) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    // 与 serve::handle 用同一套改名规则：这条壳里多一条「带不带 dk_ 前缀」的差异，
    // 同一个工具在两处就会一边能用一边报未知（AGENTS.md：行为只有一处定义）。
    let bare = name.strip_prefix("dk_").unwrap_or(name);
    if !serve::TOOLS.iter().any(|t| t.strip_prefix("dk_").unwrap_or(t) == bare) {
        return json!({
            "content": [{"type": "text", "text": format!("未知工具 {name}；可用：{}", serve::TOOLS.join(" / "))}],
            "isError": true,
        });
    }
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match serve::handle(store, bare, &args) {
        Ok(v) => json!({
            "content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}],
        }),
        Err(e) => json!({
            "content": [{"type": "text", "text": format!("{e:#}")}],
            "isError": true,
        }),
    }
}

/// 六工具对外的 schema。参数名与 \`serve::handle\` 读的键一一对应。
/// hints 只声明数组：与 DSH 插件壳、与 serve 的严格解析保持一致（一个参数一种形态，
/// 模型才不会猜；serve 侧仍宽容接受单个字符串）。
fn tool_specs() -> Value {
    json!([
        {
            "name": "dk_init",
            "description": "初始化本仓库的代码索引（建 .dk/ 与 .dkignore、写 AGENTS.md 受管块）。人触发的动作，不写卡片。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agents": {"type": "boolean", "description": "是否写入 AGENTS.md 受管块（默认 true）"}
                },
                "additionalProperties": false
            }
        },
        {
            "name": "dk_map",
            "description": "查代码索引：给 task 拿最相关的几张卡片（每张含 职责/关系/契约/约束），也可给 paths 或 dir 直接取；不给参数时给目录树。索引没建完时默认拒绝给残缺结果。confidence 只说词面是怎么对上的（surface/fallback/none），不是「答案在里面」；matched 里 hit 是首张卡踩中的查询词、of 是分母（只数本仓出现过、且不是过半卡片都有的词）、absent 是本仓一张卡都没有的词——hit 少而 absent 多说明该换词或给 paths，不是问法不好。of=0 时 hit 必为空（分母里没有有判别力的词），看 weak_n/absent_n 才知道是哪一类。命中不等于答案。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "自然语言任务描述，例如「修会话恢复时的锁竞争」"},
                    "hints": {"type": "array", "items": {"type": "string"}, "description": "补充关键词，有一条就放一条"},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "直接按路径取卡，不做检索"},
                    "dir": {"type": "string", "description": "取某目录下全部卡"},
                    "level": {
                        "type": "string",
                        "enum": ["l0", "l1", "l2"],
                        "description": "披露等级：l0 只给目录、l1 给路径加一句话、l2 给整卡（默认）"
                    },
                    "allow_partial": {"type": "boolean", "description": "索引未建完仍给结果，结果会标注不含哪些文件"},
                    "k": {"type": "integer", "description": "卡片张数（不含 R 扩散邻居），默认 6"}
                },
                "additionalProperties": false
            }
        },
        {
            "name": "dk_get",
            "description": "按路径取整卡，并顺手校验卡片记录的源码哈希是否还新鲜；同时给出这些卡在关系图里的邻居（只给路径与一句话）。不返回源码。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "仓库相对路径"}
                },
                "required": ["paths"],
                "additionalProperties": false
            }
        },
        {
            "name": "dk_put",
            "description": "写回卡片：传了哪个字段就覆盖哪个，没传的保留。源码哈希由工具自己现算，不要手填。一次调用写完这批全部文件。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "entries": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string", "description": "仓库相对路径"},
                                "F": {"type": "string", "description": "一句核心职责"},
                                "R": {"type": "string", "description": "必须一起考虑的强关系，逗号分隔 code:<路径>；无则省"},
                                "A": {"type": "string", "description": "稳定对外契约：导出签名、CLI 参数、协议字段、表结构；无则省"},
                                "S": {"type": "string", "description": "高熵约束：不写就会改错的坑；能从其它字段推出就不写"},
                                "tags": {"type": "string", "description": "可选标签，原样留存不解析"},
                                "skip": {"type": "boolean", "description": "这个文件不值得建卡：删掉它的卡并记入 .dkignore 提议区"}
                            },
                            "required": ["path"],
                            "additionalProperties": false
                        }
                    },
                    "drop_bad": {"type": "boolean", "description": "顺手清掉索引里无法解析的历史坏行"}
                },
                "required": ["entries"],
                "additionalProperties": false
            }
        },
        {
            "name": "dk_sweep",
            "description": "列出还没建卡的文件（只给路径、字节数、行数、机器特征，不给源码）。读文件、判断值不值得建卡都是你自己的事；不值得就用 dk_put 传 skip。重跑即续跑。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "本批最多几个文件，默认 10"}
                },
                "additionalProperties": false
            }
        },
        {
            "name": "dk_check",
            "description": "全量体检：受管文件数、有卡数、覆盖率、过期、失联、未建卡、坏关系、坏行、重复卡。",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }
    ])
}
