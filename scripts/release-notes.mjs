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

let body = null;
for (const row of rows) {
  const cells = row.replace(/^\||\|$/g, '').split(/\s\|\s/);
  const hit = cells.find((cell) => new RegExp('^\\*{0,2}v?' + version.replace(/\./g, '\\.') + '[：:]').test(cell));
  if (hit) body = hit;
}
if (!body) {
  console.error('SPEC.md §12 里没有 v' + version + ' 这一行：先补变更记录，再发版（Release 正文不允许手写另一份）');
  process.exit(1);
}

const notes = [
  body.replace(/^\*\*|\*\*$/g, '').trim(),
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
