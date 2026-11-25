#!/usr/bin/env node
// Release 正文的唯一来源是 SPEC.md §12 的那一行：变更与理由已经在那儿写过一次，
// 手抄一份到 Release 就是两处维护（这条账本仓库已经吃过：卡里写「六句」、实为七句）。
//
// 用法：node scripts/release-notes.mjs v0.3.5 > notes.md
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const read = (rel) => readFileSync(join(root, rel), 'utf8');

const arg = process.argv[2];
if (!arg) {
  console.error('用法：node scripts/release-notes.mjs <版本或 tag>');
  process.exit(2);
}
const version = arg.trim().replace(/^v/, '');
if (!/^\d+\.\d+\.\d+/.test(version)) {
  console.error('版本号读不出来：' + arg);
  process.exit(2);
}

// §12 的行长这样：| 2026-11-25 | v0.3.5：…… | 教训：…… |
// 「变更」列里可能带反引号与竖线，所以按「| 」切开后逐个格子找以版本号开头的那个，
// 不按列号硬取（行也曾经少过一格，按号码取会静默取错）。
const rows = read('SPEC.md')
  .split(/\r?\n/)
  .filter((line) => /^\|\s*\d{4}-\d{2}-\d{2}\s*\|/.test(line));

// 同一个版本号可能有多行（v0.3.5 就分「功能」与「发布整理」两行），全取、按文件顺序拼——
// 只留最后一条等于把另一半变更从 Release 里删掉。
const hits = [];
for (const row of rows) {
  const cells = row.replace(/^\||\|$/g, '').split(/\s\|\s/);
  // 版本号后面可能是「：」，也可能是「 发布整理：」（同一版本的第二种记法），都算这一版
  const hit = cells.find((cell) => new RegExp('^\\*{0,2}v?' + version.replace(/\./g, '\\.') + '(?=\\s|[：:]|$)').test(cell));
  if (hit) hits.push(hit.replace(/^\*\*|\*\*$/g, '').trim());
}
if (hits.length === 0) {
  console.error('SPEC.md §12 里没有 v' + version + ' 这一行：先补变更记录，再发版（Release 正文不允许手写另一份）');
  process.exit(1);
}

// §12 的一格是表格单元，里面没有换行：整版变更被压成一大面墙，贴进 Release 就是一段几百字的
// 连续文字（v0.3.5 首发就是这么出去的）。按圈号切成条目、每项一行——内容一个字不改，只把墙拆成行。
const CIRCLED = '①②③④⑤⑥⑦⑧⑨⑩⑪⑫⑬⑭⑮⑯⑰⑱⑲⑳';
function formatCell(cell) {
  const text = cell.replace(/\*\*/g, ''); // 表格里的 ** 到正文里会变成孤立标记（标题被 ** 包住时尾部会剩一个）
  const at = [...text].findIndex((ch) => CIRCLED.includes(ch));
  if (at < 0) return text.trim(); // 没有圈号：本来就是一段话，原样
  const chars = [...text];
  const title = chars.slice(0, at).join('').replace(/[—－\-]+\s*$/, '').trim();
  // 只按「下一个该出现的圈号」切：正文里会回头引「与 ① 同类」，那个 ① 不是新条目
  const items = [];
  let want = CIRCLED.indexOf(chars[at]);
  let buf = '';
  for (let i = at; i < chars.length; i++) {
    const ch = chars[i];
    const idx = CIRCLED.indexOf(ch);
    if (idx === want) {
      want++;
      if (buf.trim()) items.push(buf);
      buf = '';
    }
    buf += ch;
  }
  if (buf.trim()) items.push(buf);
  const lines = [];
  if (title) lines.push('### ' + title, '');
  for (const item of items) lines.push('- ' + item.trim().replace(/[；;]+$/, ''));
  return lines.join('\n');
}

const notes = [
  hits.map(formatCell).join('\n\n'),
  '',
  '---',
  '',
  '**安装**',
  '',
  '- 有 Rust 1.88+：`cargo install --path crates/dk --locked`，然后 `dk init`',
  '- Windows：下载 `dk-v' + version + '-x86_64-pc-windows-msvc.zip`，解压出 `dk.exe` 放进 PATH',
  '- Linux / macOS：下载对应 `dk-v' + version + '-<target>.zip`，解压出 `dk` 放进 PATH',
  '',
  '升级后 `dk init` 幂等，会把 AGENTS.md 的受管块刷成新版。完整的判据与验收标准见仓库里的 `SPEC.md`（§12 变更记录）。',
  '',
].join('\n');

process.stdout.write(notes);
