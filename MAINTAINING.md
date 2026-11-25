---
title: dk 维护手册
owner: ADkun
status: current
last_reviewed: 2026-11-25
---

# dk 维护手册

给**改这个仓库的人**。它只回答两件事：改了东西要同步哪几处、GitHub 上的 CI 与 Release 怎么维护。
协议与判据的唯一权威仍然是 `SPEC.md`；这里只写动手清单与操作步骤，不重述协议。

## 0. 三条原则

1. **一处事实，一个家。** 协议只在 `SPEC.md`；提示词只在 `package/index.js` 的 `GUIDANCE`；卡片格式只在 `SPEC.md` 的格式一节。别的文档只准指路，不准抄。
2. **能机检的别靠人记。** 版本同号、断言数量、文档预算、死链都进 CI（`scripts/version.mjs`、`scripts/check-maintain.mjs`）。红了就是真有事，不是形式主义。
3. **文档跟代码同一个提交。** 分两次提交，中间那段时间索引与文档都在说假话。

## 1. 改动同步清单

先看「你改了什么」，再看右边三列——第三列是判据，也是 CI 里判它的地方。

| 你改了什么 | 必须跟着动 | 判据（在哪判） |
| --- | --- | --- |
| 卡片字段、门限、格式 | `SPEC.md` 的格式一节、`crates/dk-core/src/store.rs` 的常量、`package/test/e2e.mjs` 的写链路断言 | `cargo test` + e2e + `dk check` 过期 0 |
| 工具名、参数、返回体 | **三处声明**：`crates/dk-core/src/serve.rs` 的 `handle`、`crates/dk-core/src/mcp.rs` 的 `tool_specs`、`package/index.js` 的 `SPECS`；外加 `SPEC.md` 的工具面一节与 `skills/dk/SKILL.md` 的用法 | e2e 的逐字一致与参数白名单断言 |
| 报错文案、`hint`、`next` | `SPEC.md` 的工具面一节；`package/test/e2e.mjs` 里「成功地空着回去」的每种形态各一条 | e2e |
| 系统提示词 `GUIDANCE` | `package/index.js`（含 `guidanceFor`）、技能副本（见 §3.4）、`package/test/prompt.test.mjs`、`SPEC.md` 的三层软提醒一节 | `node --test` + e2e 的提示词断言 |
| 技能 `skills/dk/SKILL.md` | 复制到 DSH 技能目录（见 §3.4） | 重开对话后 `/dk` 能看到新句 |
| 检索：分词、排序、兜底 | `SPEC.md` 的检索一节、`crates/dk-core/src/search.rs`、`crates/dk/examples/statbench.rs` | `cargo run --release --example statbench` 的召回数字不退化 |
| 任何文档 | `SPEC.md` 验证计划里的断言总数、`README.md` 里同一个数字 | `node scripts/check-maintain.mjs` |
| 每次改完代码 | `dk_put` 写回改过的卡（F、R 必写） | `dk check` 覆盖率 100%、过期 0 |
| 发版 | 三处版本同号 + `SPEC.md` 变更记录一行 + tag | `node scripts/version.mjs` + `release.yml` 的 verify |

三处版本是：`Cargo.toml` 的 `[workspace.package]`、`package/package.json`、`SPEC.md` 标题。

## 2. 提交前跑什么

八条验收命令在 `AGENTS.md`（agent 要能直接复制执行，所以放那儿）。这里只说为什么：
CI 的 `test` job 就是这八条里的七条，本地绿了 CI 才有理由绿。v0.3.5 那次连续两次 CI 红，
原因只是「fmt 本地能跑却没跑」——门禁的价值在于它在**本地**也跑过。

## 3. GitHub 仓库怎么维护

### 3.1 分支与 tag

- `main` 直推（单人仓库），不设 PR 流程。
- tag 只有 `vX.Y.Z` 一种。**tag 用命令行推上去，不在网页建**：Release 的资产由 tag 触发的流水线生成，网页建的 tag 不会跑流水线。
- 一个 tag 一个 Release，资产是四个平台的 zip 加一个 `SHA256SUMS.txt`。

### 3.2 CI：两个 job

| job | 跑什么 | 为什么在第一道 |
| --- | --- | --- |
| `meta`（30 秒） | 版本三处同号、发版脚本的两条失败路径、文档机检 | 这三件事不用编译就能判，不必等三平台编完才知道版本漂了 |
| `test`（三平台矩阵） | fmt、clippy、单测、真编、插件壳两条 node 测试、本仓库索引自检 | 索引自检放最后：卡要跟代码一起改，漂了就在这里红，而不是等下一次 `dk_map` 返回旧卡 |

看红：`gh run list --limit 5` 找到 run，再 `gh run view <id> --log-failed`。

### 3.3 发版四步

```bash
# 1 三处版本同号；SPEC.md 变更记录补一行（Release 正文就从这一格来）
# 2 提交
git commit -am "release: v0.3.6"
# 3 打 tag 并推（推 tag 触发 release.yml）
git tag -a v0.3.6 -m v0.3.6
git push origin main --tags
# 4 看结果
gh run list --limit 3
gh release view v0.3.6
```

- **正文不许手写**：`scripts/release-notes.mjs` 从变更记录那一格生成，按圈号切成条目（一条变更一行）。
  要改正文就改 `SPEC.md` 那一格，两处维护必然对不上——首发那次就漏了半截。
- **跑坏了不用挪 tag**：`gh workflow run release.yml --ref main -f tag=v0.3.6`。
  流水线三个 job 都把 `ref` 钉在那个 tag 上，所以能用 main 上的流水线、取该 tag 的代码重建资产。
- 下载验证：`sha256sum -c SHA256SUMS.txt`，再解压跑 `dk --version` 看是不是那一版。

### 3.4 两个「改了要同步」的副本

| 东西 | 真的那份（仓库里） | 线上读的那份 |
| --- | --- | --- |
| DSH 插件 | `package/`（profile 里是 link 进去） | link 到仓库，**改完重启 DSH 生效** |
| /dk 技能 | `skills/dk/SKILL.md` | DSH 技能目录下复制过去的那份，**改完要手动 cp，不必重启** |

技能不是 link 是副本：只改仓库里的，线上加载到的还是旧措辞。

### 3.5 权限

只需要 `GITHUB_TOKEN`（CI 只读、release 写），不需要个人 PAT。本机 `gh` 登录的账号要能直推 main。

## 4. 已知会绊人的地方

| 现象 | 原因 | 怎么办 |
| --- | --- | --- |
| `failed to remove target/release/dk.exe … (os error 5)` | 常驻的 `dk serve` 占着二进制 | 先 `Stop-Process -Name dk -Force` 再编 |
| macOS 的 job 被 cancelled 或一直 queued | `macos-13` / `macos-14` 已下线 | 用 `macos-15`（arm64）与 `macos-15-intel`（x64） |
| `actions/*` 报 Node 20 deprecated | 只是注解，GitHub 已强制 Node 24 | 忽略；升 action 主版本是另一件事 |
| `api.github.com` TLS handshake timeout | 偶发 | 重试；`gh release edit` 尤其如此 |
| `dk check` 把临时目录里的东西算成未建卡 | `_tmp` 在仓库里就受管 | 临时目录放 `$env:TEMP`，别放仓库内 |
| `dk_put` 回「超长」被拒 | 卡片字段有软门（摘要 200、契约 150、约束 400 字） | 压缩措辞，不是删信息 |

## 5. 文档地图：谁是哪个家

| 文件 | 给谁 | 说什么 |
| --- | --- | --- |
| `README.md` | 用 dk 的人 | 装、上手、常见问题 |
| `SPEC.md` | 改 dk 的人、复核者 | 协议、判据、验证计划、变更记录（唯一权威） |
| `AGENTS.md` | agent | 红线、八条验收、指路 |
| `MAINTAINING.md` | 维护者 | 这份：改动同步清单、CI 与发版操作 |
| `package/README.md` | DSH 插件的使用者 | 插件怎么配 |
| `skills/dk/SKILL.md` | 被 dk 技能管的会话 | 纪律 |

判断新内容该写在哪：**协议与判据**进 `SPEC.md`，**怎么操作**进这里，**怎么用**进 `README.md`，**agent 必须守什么**进 `AGENTS.md`。同一句话只在一个地方存在，其它地方给指针。
