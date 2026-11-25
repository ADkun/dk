# dk —— 项目约定

<!-- dk:begin -->
本仓库有代码索引 .dk/map.txt（一行一文件：职责 / 关系 / 契约 / 约束）。
· 改代码前：先 dk_map {task} 或 dk_get {paths}，不要搜完就动手。
· 改完：dk_put 写回你改过的文件（F、R 必写；A、S 没有就省）。
· 卡与源码对不上（stale）：先重读源码，再重写卡。
<!-- dk:end -->

## 这是什么

dk 是本地代码索引：`.dk/map.txt` 一行一文件，写职责、关系、契约、约束，让 agent 不读全仓也能找到该读的文件、并知道改它要注意什么。

- 协议与判据的唯一权威：`SPEC.md`
- 维护、CI、发版的操作手册：`MAINTAINING.md`
- 给用 dk 的人：`README.md`

## 五条红线

1. 核心逻辑全部在 `crates/dk-core`；CLI、`dk mcp`、DSH 插件只做参数与输出。
2. 工具行为只有一个定义处：`crates/dk-core/src/serve.rs` 的 `handle`。任何壳里都不许出现业务判断。
3. 工具名与参数有三处声明必须同步：`serve.rs` 的 `handle`、`mcp.rs` 的 `tool_specs`、`package/index.js` 的 `SPECS`。改一处就改三处。
4. 卡片格式与协议的唯一权威是 `SPEC.md`；代码注释里的 `§编号` 都指它。
5. 新增依赖前先问：这能不能用 80 行自己写完？（`tokenize.rs` 就是答案）

## 提交前跑这八条

```bash
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
node --test package/test/prompt.test.mjs
node package/test/e2e.mjs
node scripts/check-maintain.mjs
cargo build --release --locked   # 重编前先 Stop-Process -Name dk -Force（常驻 dk serve 占着二进制）
cargo run --release --locked -- check
```

CI 跑的就是这八条（少一条 `check-maintain`，它在 `meta` job）。本地绿了 CI 才有理由绿。

## 改了东西要同步哪儿

一张表在 `MAINTAINING.md` 的「改动同步清单」：改协议、改工具参数、改提示词、改技能、改断言数字各有各的必改项。别凭记忆，照着表走。

## 这个仓库不做的事

见 `SPEC.md` 的目标与非目标一节。两条最常被提的：不做跨仓库图谱（别的仓库用 `dk -C <目录> map` 只读查看），不做流程钩子（只做三层软提醒）。

## 文档地图

`README.md` 给用的人 · `SPEC.md` 给改的人 · `MAINTAINING.md` 给维护的人 · 这份给 agent。
