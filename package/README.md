# dsh-dk —— DSH 插件壳

把 [dk](../README.md) 的六个索引工具接进 DSH。**只做三件事**，没有第四件：

1. **内置拉起** `dk serve`：不需要配 `.mcp.json`，不需要自己起进程；进程死了下次调用自动重起。
2. **原生工具名**：注册成 `dk_map` 而不是 `mcp__dk__dk_map`。
3. **一段系统提示词**（约 300 字，末两个分句是跨库）：改代码前先 `dk_map` 或 `dk_get`，改完 `dk_put` 写回；要看别的仓库就 `dk -C <目录> map <任务描述>` 只读查看，别切过去写卡；那个仓库的索引不全会回 `incomplete`，那就加 `--allow-partial` 只读拿走结果，别替它补卡。它靠
   `ctx.inject(['systemPrompt'], …)` 等宿主服务就绪再注册——加载顺序反过来时会补上，不会静默丢失。

不注册任何钩子，不监听文件系统，不看工具调用。所有行为都来自 `dk serve` 的 NDJSON
（[SPEC §9.2](../SPEC.md)）——这里没有第二份逻辑，只有转发。

## 安装（本机 profile）

1. 先有二进制：`cd ../ && cargo build --release`（产出 `target/release/dk.exe`）。
2. 在 `~/.dsh/profiles/<profile>/package.json` 的 `dependencies` 加一行：

   `"dsh-dk": "link:<本仓库绝对路径>/package"`（Windows 上写 `/` 或 JSON 里转义后的双反斜杠，别用单个裸反斜杠）

3. 同一个文件的 `dsh.profile.bundles` 数组里加一行：`"dsh-dk"`。
4. `pnpm install`（在 profile 目录），重启 DSH。

## 配置（设置页或 `cordis.patch.yml`）

| 键 | 默认 | 说明 |
|---|---|---|
| `binary` | 空（自动） | 留空 = 先找本仓库 `target/{release,debug}/dk`，再走 PATH；也可写绝对路径 |
| `root` | 空 | 空 = 跟随会话目录并**向上找 `.dk/`**（其次 `.git/`）；填死则固定那个仓库 |
| `prompt` | `true` | 是否追加那一段系统提示词 |
| `callTimeoutMs` | `120000` | 单次工具调用超时 |

## 换个 harness 怎么办

不用这个插件，直接把 `dk mcp` 注册成 MCP server（stdio）即可，工具名与参数完全一致。
非 DSH、非 MCP 的场合用 `dk serve` 的 NDJSON。

## 这个壳刻意不做的事

- 不注入「你还欠一张卡」之类的状态提醒——那需要监听工具调用，边界不可穷举（[SPEC §6.1](../SPEC.md)）。
- 不缓存、不重试、不改写参数：`dk` 说错就原样报给模型，让它自己改。
- 不替你决定哪个目录是仓库：`root` 跟随会话，而不是写死在插件里。
