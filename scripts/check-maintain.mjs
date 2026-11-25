#!/usr/bin/env node
// 维护手册里那些「改了要同步」的话，能判的都在这儿判——不能判的（比如「同一事实只写一处」）才留给评审。
//
// 用法：node scripts/check-maintain.mjs        # 全部通过退出 0，有违例退出 1，脚本自身读不到东西退出 2
//
// 判四件事：
// 1. 版本三处同号（直接复用 scripts/version.mjs，别在这里再写一遍解析）
// 2. 断言总数：SPEC.md 与 README.md 里写的数字 == package/test/e2e.mjs 里真正调用的 ok() 条数
//    （数字漂过一次：SPEC 已写 v0.3.3，包还是 0.3.2，断言数 70/73 也对不上）
// 3. 文档预算：AGENTS.md ≤ 200 行、MAINTAINING.md ≤ 160 行——手册一旦变成第二本 SPEC 就没人读
// 4. 引用可查：维护类文档里反引号写的仓库内路径必须存在；不许写「文件:行号」（行号会烂）
//    三处工具声明是否逐字一致不在这里判：e2e 真起 `dk mcp` 对拍过（比读源码正则可靠）
import { existsSync, readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const read = (rel) => readFileSync(join(root, rel), 'utf8');
const bad = [];

// 1. 版本三处同号
const ver = spawnSync(process.execPath, [join(root, 'scripts/version.mjs')], { encoding: 'utf8' });
if (ver.status !== 0) bad.push('版本三处不同号：' + (ver.stderr || ver.stdout).trim().split('\n').join(' | '));

// 2. 断言总数
// 只认「当前」那一处数字：SPEC.md 的「N 条断言全过」，README.md 的命令注释行。
// §10 与 §12 里还有一堆历史数字（「32 → 33」「原 26 条」），那些是记录，不是判据，不能一起比。
const e2e = read('package/test/e2e.mjs');
const actual = (e2e.match(/(^|[^\w.])ok\(/g) || []).length;
const current = {
  'SPEC.md': /(\d+) 条断言全过/,
  'README.md': /(\d+) 条断言/,
};
for (const [file, re] of Object.entries(current)) {
  const m = read(file).match(re);
  if (!m) bad.push(file + ' 里找不到断言总数的当前值（判据是「N 条断言全过」/「N 条断言」）');
  else if (Number(m[1]) !== actual) bad.push(file + ' 写的是 ' + m[1] + ' 条，e2e.mjs 实际调用 ok() ' + actual + ' 次');
}

// 3. 文档预算
const budget = { 'AGENTS.md': 200, 'MAINTAINING.md': 160 };
for (const [file, limit] of Object.entries(budget)) {
  const lines = read(file).split('\n').length;
  if (lines > limit) bad.push(file + ' 有 ' + lines + ' 行，超过预算 ' + limit + ' 行：内容该下沉到 SPEC.md 或删');
}

// 4. 引用可查
const docs = ['AGENTS.md', 'MAINTAINING.md', 'README.md'];
const PATH_RE = /^[\w.\-/]+\.(md|rs|mjs|js|toml|yml|yaml|json)$/;
for (const file of docs) {
  const text = read(file);
  for (const m of text.matchAll(/`([^`\n]+)`/g)) {
    const token = m[1].trim();
    if (!PATH_RE.test(token) || !token.includes('/')) continue;
    // 只判「以本仓库真实顶层目录开头」的路径：README 里的 src/a.rs 是示意路径，不是这里的文件
    const top = token.split('/')[0];
    if (!existsSync(join(root, top))) continue;
    if (!existsSync(join(root, token))) bad.push(file + ' 引用的路径不存在：' + token);
  }
  // 行号引用：文档里钉行号，代码一动就谎报
  const lineRef = text.match(/[\w.\-/]+\.(?:md|rs|mjs|js|toml|yml):\d+/);
  if (lineRef) bad.push(file + ' 里写了行号引用（' + lineRef[0] + '）：改成写函数名或小节名');
}

// 5. 每个 .md 都要在维护手册的文档地图里有一席之地，否则新增的文档没人知道它在哪一层
const map = read('MAINTAINING.md');
for (const file of ['AGENTS.md', 'MAINTAINING.md', 'README.md', 'SPEC.md', 'package/README.md', 'skills/dk/SKILL.md']) {
  if (!map.includes('`' + file + '`')) bad.push('MAINTAINING.md 的文档地图里没有 ' + file);
}

if (bad.length) {
  for (const line of bad) console.error('✗ ' + line);
  console.error('\n维护一致性检查未通过：' + bad.length + ' 条');
  process.exit(1);
}
console.log('维护一致性 OK：版本三处同号、断言 ' + actual + ' 条对得上、文档预算与引用可查');
