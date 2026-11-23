# dk —— 项目约定

<!-- dk:begin -->
本仓库有代码索引 .dk/map.txt（一行一文件：职责 / 关系 / 契约 / 约束）。
· 改代码前：先 dk_map {task} 或 dk_get {paths}，不要搜完就动手。
· 改完：dk_put 写回你改过的文件（F、R 必写；A、S 没有就省）。
· 卡与源码对不上（stale）：先重读源码，再重写卡。
<!-- dk:end -->

## 开发约定

- 核心逻辑全部在 `crates/dk-core`，CLI 与将来的 MCP / DSH 壳只做参数与输出。
- 工具行为只有一个定义处：`crates/dk-core/src/serve.rs` 的 `handle`。CLI、`dk serve`、
  `dk mcp`、`package/index.js` 都只是它的壳，任何壳里都不许出现业务判断。
- 工具名与参数有三处声明必须同步：`serve.rs` 的 `handle`、`mcp.rs` 的 `tool_specs`、
  `package/index.js` 的 `SPECS`。改一处就改三处。
- 卡片格式与协议的唯一权威是 `SPEC.md`；代码注释里的 `§编号` 都指它。
- 新增依赖前先问：这能不能用 80 行自己写完？（`tokenize.rs` 就是答案）
