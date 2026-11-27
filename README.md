<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <img src="assets/logo.svg" width="76" height="76" alt="dk">
  </picture>
</p>

<h1 align="center">dk —— 轻量代码索引</h1>

<p align="center">
  给 AI agent 用的代码索引：一个仓库一份 <code>.dk/map.txt</code>，一行一个文件，<br>
  写清四件事 —— <b>职责 · 关系 · 契约 · 约束</b>。改代码前查它，改完写回——<b>没有阶段、没有批次、没有审批流</b>。
</p>

<p align="center"><sub>简体中文 · <a href="README.en.md">English</a></sub></p>

<p align="center">
  <a href="https://github.com/ADkun/dk/actions/workflows/ci.yml"><img src="https://github.com/ADkun/dk/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <a href="Cargo.toml"><img src="https://img.shields.io/badge/rust-1.88%2B-orange.svg" alt="Rust 1.88+"></a>
</p>

> 把「这个仓库长什么样」从每次重新发现，变成一份**可以带走、可以机检**的索引。

<p align="center">
  <a href="https://github.com/ADkun/dk/releases/download/v0.3.6/dk-promo.mp4">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="assets/promo-cover-dark.png">
      <img src="assets/promo-cover.png" width="720" alt="dk 介绍片：56 秒，看它怎么把碎片拼成一张地图">
    </picture>
  </a><br>
  <sub><a href="https://github.com/ADkun/dk/releases/download/v0.3.6/dk-promo.mp4">▶ 看 56 秒介绍片</a></sub>
</p>

> **目录**：[这是什么](#这是什么) · [为什么](#为什么值得用) · [安装](#安装) · [30 秒上手](#30-秒上手) · [卡片格式](#卡片长什么样) · [六个工具](#六个工具) · [CLI](#cli) · [设计取舍](#三条硬边界) · [对比](#和-aoci-比) · [项目结构](#项目结构) · [自测](#开发与自测) · [License](#license)

## 这是什么

dk 不代替 agent 读源码，只回答一个问题：**该读哪个文件，为什么。**

一个仓库一份纯文本索引 `.dk/map.txt`，一行一个文件，每行写清四件事 —— **F 职责、R 关系、A 契约、S 约束**。

工作循环只有三步：

1. **改代码前** —— `dk_map` 按任务查到该读的文件；已知路径就用 `dk_get` 取全卡。
2. **改完** —— `dk_put` 把动过的文件写回，源码指纹自动重算。
3. **收工 / CI** —— `dk_check` 全量体检：覆盖、过期、失联、坏行一次看清。

四个字段的精确含义，见[下一节](#卡片长什么样)。

## 为什么值得用

大仓库里 agent 改一次代码，成本大头不是读文件，是**找文件**：每次会话都从 grep 开始，把已经弄清的结构再弄清一遍。

- **痛点** —— 传统方案（快照、基线、审批、证据链）把「写一行摘要」变成了「跑一遍流程」，边际成本高过重读源码，于是没人维护、索引烂掉。
- **取舍** —— 一次改动的边际成本，必须低于它省下的搜索成本。所以 dk 只有六个工具、一份纯文本索引、一次写入。
- **底线** —— 治理强度靠「卡写得准 + 随时可取 + 可机器验证」，不靠「证明治理发生过」。

## 安装

dk 本体是一个 CLI；接进 Harness 时，DSH 用插件，其它 Harness 用 MCP。三者背后是同一份逻辑，不需要分别配规则。

| 你要做的 | 何时需要 |
| --- | --- |
| [装 CLI](#装-cli) | 所有场景的第一步 |
| [接入 DSH](#接入-dsh) | 用 DSH，想让六个工具以原生名出现 |
| [注册 MCP](#接入其它-harnessmcp) | 用其它支持 MCP 的 Harness |
| 放一份 `/dk` 技能 | 想要一层软提醒 |

### 装 CLI

**从 Release 装（推荐）**：到 [Releases](https://github.com/ADkun/dk/releases) 下载 `dk-v<版本>-<target>.zip`（tag 带 `v`，例如 `v0.3.6`），按系统 / 架构挑好解压，把 `dk` / `dk.exe` 放进 `PATH` 里的任一目录，验证：

```bash
dk --version
```

**从源码装**：需要 Rust 1.88+（rustup 会把 `~/.cargo/bin` 放进 `PATH`）：

```bash
git clone https://github.com/ADkun/dk.git
cd dk
cargo install --path crates/dk --locked
dk --version
```

只想临时试跑、不写进 `PATH`：`cargo build --release --locked`，二进制在 `target/release/dk`（Windows 是 `dk.exe`）。

### 接入 DSH

插件只做三件事：内置拉起 `dk serve`、注册原生工具名、追加一段系统提示词。先装好 CLI，再改 profile：

```text
~/.dsh/profiles/<profile>/package.json
  dependencies:        "dsh-dk": "link:<本仓库绝对路径>/package"
  dsh.profile.bundles: 追加 "dsh-dk"
```

Windows 路径写 `/` 或 JSON 里转义后的双反斜杠，别用单个裸反斜杠。然后在 profile 目录 `pnpm install`，重启 DSH。

两个默认值让换机器也能跑：

- `binary` 留空 —— 先找本仓库的 `target/{release,debug}/dk`，再走 `PATH`。
- `root` 留空 —— 从会话目录向上找 `.dk/`（其次 `.git/`），`cd` 进子目录也不会分裂索引。

全部配置项见 [package/README.md](package/README.md)。

再放一份技能副本，DSH 里的 `/dk` 才会生效：把 `skills/dk/SKILL.md` 复制到 `~/.dsh/skills/dk/SKILL.md`。必须是**复制，不是链接**；改了仓库那份要重新同步，否则线上会静默落后。

### 接入其它 Harness（MCP）

不用插件：把 `dk mcp` 注册成一个 stdio MCP server 即可。工具名见[六个工具](#六个工具)，参数由 MCP `tools/list` 给出。各 Harness 的配置字段名不同，按它的格式翻译这一组值：

```json
{
  "mcpServers": {
    "dk": {
      "command": "dk",
      "args": ["-C", "<仓库根目录的绝对路径>", "mcp"]
    }
  }
}
```

- `command` 写 `dk.exe` 或二进制绝对路径也行，只要 Harness 能拉起它。
- `-C` 是全局选项，固定仓库根；只有 Harness 保证以仓库根启动 server 时才能省略——dk 自己不上溯。
- 进程由 Harness 按 MCP stdio 约定管理，不需要自己起服务。
- 若 Harness 支持自定义技能，`skills/dk/SKILL.md` 是普通 Markdown，按它的技能目录约定放一份即可；这一步不影响 MCP。

## 30 秒上手

装好后（见[安装](#安装)），在要索引的仓库根跑：

```bash
cd <你的仓库>
dk init                                    # 建 .dk/ 与 .dkignore，并往 AGENTS.md 追加受管块
dk sweep                                   # 看还差哪些文件没建卡（只列路径与机器特征，不读源码）
echo '{"entries":[{"path":"AGENTS.md","F":"项目约定与 dk 受管块"}]}' | dk put   # 写卡；省略文件读 stdin
dk map "改会话恢复时的锁竞争"               # 按任务检索卡片，无参给目录树
dk get AGENTS.md                           # 取全卡 + 同目录兄弟 + 新鲜度
dk check                                   # 全量体检；不干净时 exit 1（--json 给机器读，只此一条命令有）
```

> **两条容易踩的边界**
>
> - 写入的 `path` 必须是**已存在且受管**的文件，所以上面拿 `AGENTS.md` 演示；随手编一个 `src/a.rs` 而仓库里没有，会被拒。
> - `dk init` 写的 `AGENTS.md` 自身也受管，所以最小仓库第一次 `dk check` 会报「受管 2 文件 / 1 张卡 / 50.0%」。给剩下那个文件写卡（或 `dk_put` 传 `skip:true`）就回到 100%——这不是坑，是[硬边界 1](#三条硬边界)要的：没有文件能悄悄绕过索引。

模型侧不用敲命令：六个 MCP 工具，一张卡一次调用。

## 卡片长什么样

```text
<path>@<源码h8>[<tags>]: F:<职责> | R:<关系> | A:<契约> | S:<约束> #<行校验h8>
```

| 字段 | 内容 | 判据 |
| --- | --- | --- |
| **F** | 一句话职责 | 读完能说出「这文件干什么」，而不是「它有什么」 |
| **R** | 必须一起考虑的文件（逗号分隔，可含 `database://`） | 改这里而不看那里就会错 |
| **A** | 对外契约：导出签名、CLI 参数、协议字段、表结构 | 它一变，调用方就得变 |
| **S** | 系统约束 | 能从其它字段推出来吗？不写会导致改错吗？都能就别写 |

两个哈希让它能自校验：

- 行尾 `#h8` —— 这一行正文的 blake3 前 8 位，手改坏了当场报错，不静默通过。
- `@<源码h8>` —— 源码指纹，`dk check` 用它判过期；不需要基线快照，指纹现算。

> **错的 S 比缺的 S 更有害**：一条过期约束会让下一个模型做出错误决定，而缺一条只会让它多读一次文件。

## 六个工具

一份逻辑，三个壳（CLI / MCP / DSH 插件）共用 `dk serve` 的 NDJSON 入口。

| 工具 | 干什么 | 何时用 |
| --- | --- | --- |
| `dk_init` | 建 `.dk/` 与 `.dkignore`，写 AGENTS.md 受管块 | 人触发一次，不写卡 |
| `dk_sweep` | 列出还没建卡的文件（路径 + 机器特征，不带源码） | 建库时按批推进 |
| `dk_map` | 按任务描述检索卡片；无参给目录树 | **改代码前第一步** |
| `dk_get` | 按路径取全卡，附同目录兄弟与新鲜度 | 已知要改哪个文件 |
| `dk_put` | 写卡：`{entries:[{path,F,R,A,S}]}`，哈希由工具现算；改名用 `from`、删卡用 `remove`/`skip`，批量前可 `dry_run` | 改完立刻写回 |
| `dk_check` | 全量体检：覆盖率、过期、失联、坏行、坏 R，另报软门告警与可修复坏行（`--fix` 就地重签） | 收工前 / CI 里 |

一次检索长什么样（本仓库几十个文件，节选前两张卡、正文有截断）：

```text
$ dk map "插件壳怎么装到 profile"
MAINTAINING.md@82521e90: F:维护手册：改了东西要同步哪几处、CI 与发版怎么操作、五条已知坑 | R:… | A:…
Cargo.toml@40629ac2: F:工作区：两个 crate 与统一 release 档 | R:crates/dk/Cargo.toml, crates/dk-core/Cargo.toml | A:…
（首张卡命中 3/4 个有判别力的查询词：插件、怎么、profile）
```

> 示意：`@` 后的源码指纹与卡片排序会随索引变化，实际以 `dk map` 现查为准。两条卡合计约 1 k token，替掉的是「把每个文件挨个打开、判断哪个与这次改动相关」。

> 末行是「这次问法有多虚」的凭据：`confidence`（词怎么对上的）、`matched`（命中了哪些查询词）、`absent`（本仓没有的词）——语义与判据见 [SPEC.md](SPEC.md)。问法不准时，补 `hints`（放源码 / 报错里的标识符）或直接给 `paths`，比挨个看候选猜哪个对快得多。
>
> 换个英文问法，`absent` 会把不重合的英文词直接列出来——不是工具不认英文，是英文词与中文卡片本就没有重叠。

## CLI

```text
init    sweep   check   get   put   map   tokens   serve   mcp
```

- `serve` —— NDJSON，一行一帧 `{id,tool,args}` → `{id,ok,result|error}`（DSH 插件与自建 harness 走这个）
- `mcp` —— MCP stdio 服务器（JSON-RPC 2.0），给支持 MCP 的 agent 直接注册
- `tokens` —— 统计卡片体量与 token 估算，判断索引是否已经贵到该拆
- `-C <dir>` —— 指定仓库根，默认当前目录。dk 自己不上溯——上溯是壳的职责（见 [package/README.md](package/README.md)）

## 三条硬边界

1. **覆盖要么 100%，要么明说没建完。** `dk_map` 的任务检索在还差 N 个文件时拒绝返回结果（`allow_partial:true` 才给，且结果头部永久标出缺哪些）；目录树 / 按路径 / 按目录三种查询只回答「索引里有什么」，不拦也不排序。部分索引比没有索引更危险：它让人以为查过了。

2. **不做流程强制。** AGENTS.md 一块、系统提示词一段、`/dk` 技能一个——就这三层软提醒。dk 不看你的工具调用、不监听文件系统、不挂压缩钩子。钩子要穷举 OS × harness × 文件语义 × 读次数 × 压缩五个维度，不可穷举即不可信，而不可信的治理工具比没有更糟。

3. **索引是可删的。** 删掉 `.dk/` 只会让你回到「没有索引」，不会留下不一致的半成品。

## 和 AOCI 比

思想同源（渐进式披露：先给索引，再给文件），流程砍到只剩写入：

| | [AOCI](https://github.com/ADkun/aoci-code) | dk |
| --- | --- | --- |
| 一次小改动的边际成本 | rules→…→check：15 KB 响应、7 个往返 | `dk_map`→`dk_get`→`dk_put`：约 1 KB、1–3 个往返 |
| 会话常驻 | ≈ 8.5 k token | 0（一次 `dk_map` ≈ 1 k token，用完即走） |
| 第一次建库 | 200 万 token 级 | 与卡片数成正比，一次一张 |
| 变化判定 | baseline 快照 + 三层派生身份 + recovery 事务 | 源码 blake3 前 8 位，现算 |
| 治理证据链 | ledger / receipt / attestation / 审批 | 无。卡与源码一致就是对，不一致就是错 |

dk 不证明「治理发生过」，只让改错的机会变少。

## 排除规则

`.dkignore`（`.gitignore` 语法）是唯一的排除声明，人写在上面；工具判定「不值得建卡」的精确路径追加到文件末尾的 `# dk:proposed` 分节，人可整段删除。

内置排除四类：

- 七个目录 —— `.git` `.dk` `node_modules` `target` `.venv` `__pycache__` `.idea`
- 二进制扩展名
- 单个大于 1 MiB 的文件（大文件不该用摘要代替阅读）
- `.dkignore` 自身

`dist` / `build` / `vendor` 不内置——不同生态含义不同，请自己写进 `.dkignore`。

## 项目结构

```text
crates/dk-core/   全部逻辑：卡片格式、存储、检索、体检、MCP 壳
crates/dk/        CLI（只做参数与输出）
package/          DSH 插件壳（原生工具名 + 内置拉起 + 一段提示词）
skills/dk/        /dk 技能：发现模型跳过索引时，强制注入纪律
SPEC.md           唯一权威规格：卡片格式、工具面、检索、硬门、三壳
MAINTAINING.md    维护手册：改了东西要同步哪几处、CI 与发版怎么操作
AGENTS.md         给 agent 的根入口：五条红线 + 提交前八条命令
.dk/map.txt       本仓库自己的索引（dogfooding）
scripts/          发版用的小脚本：版本三处同号、Release 正文取自 SPEC §12
.github/workflows/ CI（三平台门禁）与 Release（打 tag 自动发版）
```

工具行为只有**一个定义处**：`crates/dk-core/src/serve.rs` 的 `handle`。CLI、`dk serve`、`dk mcp`、DSH 插件都只是它的壳，任何壳里都不许出现业务判断。

## 开发与自测

```bash
cargo test                                 # 单元测试
cargo clippy --all-targets -- -D warnings
cargo fmt --check
node scripts/version.mjs                   # 版本号三处同号（CI 的第一道就是它）
node --test package/test/prompt.test.mjs   # 插件壳单元测试（需先 cargo build）
node package/test/e2e.mjs                  # 插件壳端到端：fake ctx 跑真链路，75 条断言
node scripts/check-maintain.mjs            # 维护一致性：版本同号 / 断言总数 / 文档预算 / 引用可查
dk check                                   # 本仓库索引是否一致（CI 最后一步就是它）
```

## 发版与状态

发版、CI、以及「改了东西要同步哪几处」，都在 [MAINTAINING.md](MAINTAINING.md)——那边是唯一一份维护手册，这里不写第二份：两处维护必然对不上。

早期版本（v0.3.7），接口已稳定、细节仍会动。规格里标了 M4 的章节是**未实现的设计**，不当现状用——文档写未实现的实现，比缺文档更危险。

## License

MIT，见 [LICENSE](LICENSE)。
