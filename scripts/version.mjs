#!/usr/bin/env node
// 版本号只有一个真相：Cargo.toml 的 [workspace.package] version。
// 另外两处（package/package.json、SPEC.md 标题）是它的镜子——本仓库漂过一次
// （SPEC 已写 v0.3.3，包还报 0.3.2，dk --version 也是 0.3.2），所以让 CI 与发版都守这一条。
//
// 用法：
//   node scripts/version.mjs                  # 三处必须一致，打印版本
//   node scripts/version.mjs --expect v0.3.5  # 额外要求等于这个 tag（发版用；v 前缀可省）
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const read = (rel) => readFileSync(join(root, rel), 'utf8');

function cargoVersion() {
  const manifest = read('Cargo.toml');
  const at = manifest.indexOf('[workspace.package]');
  if (at < 0) throw new Error('Cargo.toml 里没有 [workspace.package]');
  const section = manifest.slice(at + '[workspace.package]'.length).split(/\n\s*\[/)[0];
  const m = /^\s*version\s*=\s*"([^"]+)"/m.exec(section);
  if (!m) throw new Error('Cargo.toml 的 [workspace.package] 里没有 version');
  return m[1];
}

function specVersion() {
  const title = read('SPEC.md').split(/\r?\n/, 1)[0];
  const m = /SPEC v(\d+\.\d+\.\d+)/.exec(title);
  if (!m) throw new Error('SPEC.md 第一行标题里没有「SPEC vX.Y.Z」：' + title);
  return m[1];
}

const sources = [
  ['Cargo.toml [workspace.package].version', cargoVersion],
  ['package/package.json version', () => JSON.parse(read('package/package.json')).version],
  ['SPEC.md 标题', specVersion],
];

const values = sources.map(([name, get]) => {
  try {
    return { name, value: get() };
  } catch (err) {
    console.error('读不出 ' + name + '：' + err.message);
    process.exit(2);
  }
});

const distinct = [...new Set(values.map((v) => v.value))];
if (distinct.length !== 1) {
  console.error('版本号不一致——改一处就得改三处：');
  for (const v of values) console.error('  ' + v.name + ' = ' + v.value);
  process.exit(1);
}

const version = distinct[0];
const expectAt = process.argv.indexOf('--expect');
if (expectAt >= 0) {
  const raw = process.argv[expectAt + 1];
  if (!raw) {
    console.error('--expect 后面要跟 tag（如 v0.3.5）');
    process.exit(2);
  }
  const want = raw.trim().replace(/^v/, '');
  if (want !== version) {
    console.error('tag ' + raw + ' 与代码里的 ' + version + ' 不一致：发版前先把三处版本号改成 ' + want);
    process.exit(1);
  }
}

console.log(version);
