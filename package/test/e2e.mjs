/**
 * fake-ctx 端到端：不启动 DSH，直接按插件契约模拟 ctx，跑完插件壳的真实链路。
 * 断言五件事：①Config 探测失败不炸 ②六个工具按原生名注册 ③execute 转发到 dk serve 并拿到结果
 * ④系统提示词只加一段、order/文案符合 SPEC §6 ⑤换 root 会重启子进程。
 *
 * 前置：cargo build（debug 即可）。二进制可用环境变量 DK_BIN 指定。
 * 运行：node package/test/e2e.mjs（它是端到端脚本，不是 *.test.mjs，node --test 不会自动收它）
 */
import { mkdirSync, writeFileSync, existsSync, rmSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

// 三条路径全部从本文件位置推导：仓库换了盘符或克隆到别的目录都能跑。
const HERE = dirname(fileURLToPath(import.meta.url))          // <repo>/package/test
const SELF = join(HERE, '..', '..')                            // <repo>
const PLUGIN = join(SELF, 'package', 'index.js')
const exe = process.platform === 'win32' ? 'dk.exe' : 'dk'

function pickBinary() {
  if (process.env.DK_BIN) return process.env.DK_BIN
  for (const profile of ['release', 'debug']) {
    const candidate = join(SELF, 'target', profile, exe)
    if (existsSync(candidate)) return candidate
  }
  return exe   // 退到 PATH
}
const BIN = pickBinary()
// 临时仓库放在 target/ 下（已被 .gitignore 排除）：系统临时目录在部分沙箱里不可写。
const TEMP = join(SELF, 'target', 'e2e-tmp')

let pass = 0, fail = 0
const ok = (label, cond, extra = '') => {
  if (cond) { pass++; console.log('  PASS ' + label + (extra ? ' — ' + extra : '')) }
  else { fail++; console.log('  FAIL ' + label + (extra ? ' — ' + extra : '')) }
}

function makeCtx(root) {
  const tools = new Map(), sections = [], effects = []
  const prompt = { section(spec) { sections.push(spec); return () => {} } }
  const ctx = {
    tools: { register(def) { tools.set(def.name, def); return () => tools.delete(def.name) } },
    effect(fn) { const d = fn(); effects.push(d); return d },
    get(key) {
      if (key === 'workingDirectory') return { ensure: async () => root }
      if (key === 'systemPrompt') return prompt
      return undefined
    },
    // 插件用 ctx.inject(['systemPrompt'], cb) 惰性等依赖（见 index.js 注释），
    // fake ctx 必须实现它，否则那一句提示词会在 try/catch 里被静默跳过。
    inject(keys, cb) {
      const child = { systemPrompt: prompt, get: ctx.get, tools: ctx.tools, effect: ctx.effect }
      const dispose = cb(child)
      return { dispose: () => { try { dispose && dispose() } catch { /* 已拆 */ } } }
    },
  }
  return { ctx, tools, sections, effects }
}

const disposeAll = (...groups) => {
  for (const g of groups) for (const d of g.effects) { try { d && d() } catch { /* 已拆 */ } }
}

const mod = await import(pathToFileURL(PLUGIN).href)
console.log('== 插件元信息 ==')
ok('name', mod.name === 'dsh-dk', mod.name)
ok('inject 只要 tools', Array.isArray(mod.inject) && mod.inject.length === 1 && mod.inject[0] === 'tools', JSON.stringify(mod.inject))
ok('Config 要么是 schemastery 描述符要么是 undefined，不抛错',
  mod.Config === undefined || typeof mod.Config === 'object' || typeof mod.Config === 'function',
  'typeof = ' + typeof mod.Config)

const { ctx, tools, sections, effects } = makeCtx(SELF)
mod.apply(ctx, { binary: BIN })

const EXPECT = ['dk_init', 'dk_map', 'dk_get', 'dk_put', 'dk_sweep', 'dk_check']
console.log('== 工具注册 ==')
ok('注册六个工具', tools.size === 6, [...tools.keys()].join(','))
ok('名字与原生命名一致', EXPECT.every(n => tools.has(n)))
for (const [name, def] of tools) {
  if (!def.parameters || def.parameters.type !== 'object') { fail++; console.log('  FAIL ' + name + ' 缺 parameters') }
  if (typeof def.output?.render !== 'function') { fail++; console.log('  FAIL ' + name + ' 缺 output.render') }
}
ok('写工具声明为不并发安全', tools.get('dk_put').isConcurrencySafe() === false && tools.get('dk_init').isConcurrencySafe() === false)
ok('读工具声明为并发安全', tools.get('dk_map').isConcurrencySafe() === true && tools.get('dk_check').isConcurrencySafe() === true)

console.log('== 系统提示词 ==')
ok('只加一段', sections.length === 1, String(sections.length))
ok('order = 11500', sections[0]?.order === 11500, String(sections[0]?.order))
ok('不含 harness 工具名（read/write/edit/ask_user）',
  !/read|write|edit|ask_user/.test(sections[0]?.text.replace(/dk_\w+/g, '') || 'x'))
ok('文案里点名 dk_map / dk_put', /dk_map/.test(sections[0]?.text || '') && /dk_put/.test(sections[0]?.text || ''))

const exec = { agent: {}, signal: undefined }

console.log('== 只读链路（本仓库）==')
const check = await tools.get('dk_check').execute({}, exec)
ok('dk_check 返回对象', typeof check === 'object' && check !== null, JSON.stringify(check).slice(0, 120))

const map = await tools.get('dk_map').execute({ task: '检索怎么算相关度' }, exec)
ok('dk_map 有命中', Array.isArray(map.cards) || Array.isArray(map.dirs) || map.status, JSON.stringify(map).slice(0, 200))
const rendered = tools.get('dk_map').output.render({}, map)
ok('output.render 产出 text', rendered.length === 1 && rendered[0].type === 'text' && rendered[0].text.includes('{'))

const get = await tools.get('dk_get').execute({ paths: ['crates/dk-core/src/search.rs'] }, exec)
const state = Array.isArray(get.cards) ? get.cards[0]?.state : get.state
ok('dk_get 给出新鲜度', state === 'fresh' || state === 'stale', String(state))

console.log('== 参数互斥：不替调用方猜，也不静默丢弃（§3）==')
// 审查实测过的三种「问了等于没问」：task+paths 时 task 被吞、只给 hints 退成目录树、
// dir+level:"l0" 照样给全卡；再加 k:0 让 confidence/matched 说假话。全部必须报错。
const errOf = async (args) => {
  try { await tools.get('dk_map').execute(args, exec); return '' } catch (e) { return String(e?.message || e) }
}
ok('task 与 paths 同给 → 报错', /只能给一个/.test(await errOf({ task: '检索', paths: ['crates/dk-core/src/serve.rs'] })))
ok('paths 与 dir 同给 → 报错', /只能给一个/.test(await errOf({ paths: ['README.md'], dir: 'crates' })))
ok('只给 hints 不给 task → 报错', /只属于任务检索/.test(await errOf({ hints: ['bm25'] })))
ok('dir 带 level → 报错', /只属于任务检索/.test(await errOf({ dir: 'crates/dk-core/src', level: 'l0' })))
ok('k:0 → 报错（否则命中被裁空，confidence 说假话）', /至少 1/.test(await errOf({ task: 'bm25', k: 0 })))
// schema 写了 additionalProperties:false，实现就得真拒：拼错的 hint 被静默忽略，比报错难查得多。
ok('拼错的参数 → 报错而不是静默忽略', /未知参数 hint/.test(await errOf({ task: 'bm25', hint: ['检索'] })))

console.log('== 壳与核心的约定：CLI 也要能用（v0.3.2 的 F1）==')
// e2e 原先只走 serve 路径，于是互斥守卫把 CLI 唯一的入口用法（无参给目录树）打死时，
// 33 条断言一条都没发现。壳与核心的约定必须两端各测一次。
const { execFileSync } = await import('node:child_process')
const cli = (args) => {
  try {
    return { out: execFileSync(BIN, args, { encoding: 'utf8' }), err: '' }
  } catch (e) {
    return { out: String(e.stdout || ''), err: String(e.stderr || e.message) }
  }
}
const tree = cli(['map', '-C', SELF])
ok('裸 dk map 出目录树（clap 默认值不许转发成"给了 level"）',
  /只给目录形状/.test(tree.out) && tree.out.includes('crates/'), (tree.out.split(String.fromCharCode(10))[1] || tree.err).trim())
const noTaskLevel = cli(['map', '-C', SELF, '--level', 'l0'])
ok('没有 task 却给 --level l0 → 报错，不静默忽略', /只属于任务检索/.test(noTaskLevel.err), noTaskLevel.err.trim().slice(0, 80))
ok('空 paths → 报错（成功但空等于没问）', /paths 不能为空/.test(await errOf({ paths: [] })))

console.log('== 第二个 root：写链路（临时目录）==')
rmSync(TEMP, { recursive: true, force: true })
mkdirSync(join(TEMP, 'src'), { recursive: true })
writeFileSync(join(TEMP, 'src/alpha.rs'), 'pub fn alpha() -> u32 { 1 }\n')
writeFileSync(join(TEMP, 'src/beta.rs'), 'pub fn beta() -> u32 { 2 }\n')
const second = makeCtx(TEMP)
const secondMod = await import(pathToFileURL(PLUGIN).href + '?v=3')
// 临时仓库显式给 root：它在 <repo>/target/ 下，不给 root 时会按设计上溯到本仓库（见下一组）。
secondMod.apply(second.ctx, { binary: BIN, root: TEMP })
const toolsB = second.tools
const execB = { agent: {}, signal: undefined }

// init 只建 .dk/ 与 .dkignore：空索引没有意义，map.txt 由第一次 dk_put 落盘。
// agents:false 让这片临时仓库不写 AGENTS.md，受管集合就只剩两个源文件。
const init = await toolsB.get('dk_init').execute({ agents: false }, execB)
ok('dk_init 建出 .dk/ 与 .dkignore，且 --no-agents 不碰 AGENTS.md',
  existsSync(join(TEMP, '.dk')) && existsSync(join(TEMP, '.dkignore')) && !existsSync(join(TEMP, 'AGENTS.md')),
  JSON.stringify(init).slice(0, 160))
const covB = await toolsB.get('dk_check').execute({}, execB)
ok('新实例的 root 跟着 ctx 走（受管 2 文件）', Number(covB.managed) === 2, 'managed=' + covB.managed)

const sweep = await toolsB.get('dk_sweep').execute({ limit: 5 }, execB)
ok('dk_sweep 认出新文件', Array.isArray(sweep.batch) && sweep.batch.length === 2, JSON.stringify(sweep).slice(0, 160))

const one = await toolsB.get('dk_put').execute({ entries: [{ path: 'src/alpha.rs', F: '算出 alpha 常量', R: 'code:src/beta.rs', S: '别把 1 改成 2' }] }, execB)
ok('dk_put 写一张卡', Array.isArray(one.added) && one.added.length === 1, JSON.stringify(one).slice(0, 160))

const half = await toolsB.get('dk_map').execute({ task: 'alpha 常量' }, execB)
ok('覆盖率 1/2 时拒绝给检索结果（§3 不完整即拒绝）', half.status === 'incomplete' && half.uncarded === 1, JSON.stringify(half).slice(0, 180))

const partial = await toolsB.get('dk_map').execute({ task: 'alpha 常量', allow_partial: true }, execB)
ok('allow_partial 放行时带 coverage 与 partial 声明', Array.isArray(partial.cards) && typeof partial.partial === 'string' && partial.partial.includes('src/beta.rs'), JSON.stringify(partial).slice(0, 180))

const two = await toolsB.get('dk_put').execute({ entries: [{ path: 'src/beta.rs', F: '算出 beta 常量' }] }, execB)
ok('dk_put 补第二张卡', Array.isArray(two.added) && two.added.length === 1, JSON.stringify(two).slice(0, 160))

const after = await toolsB.get('dk_check').execute({}, execB)
ok('写后 check 覆盖 2/2 且 complete', Number(after.cards) === 2 && after.complete === true, JSON.stringify({ cards: after.cards, managed: after.managed, complete: after.complete }))

const mapB = await toolsB.get('dk_map').execute({ task: 'alpha 常量' }, execB)
ok('覆盖完整后同一进程内立刻能查到', Array.isArray(mapB.cards) && mapB.cards.length > 0, JSON.stringify(mapB).slice(0, 140))

console.log('== 跟着会话目录走：不给 root 时上溯到仓库根 ==')
const up = makeCtx(join(TEMP, 'src'))
const upMod = await import(pathToFileURL(PLUGIN).href + '?v=4')
upMod.apply(up.ctx, { binary: BIN })
const upInit = await up.tools.get('dk_init').execute({}, { agent: {}, signal: undefined })
ok('子目录作为工作目录时落到有 .dk/ 的那一层（不分裂索引）',
  String(upInit.index || '') === join(TEMP, '.dk', 'map.txt'),
  String(upInit.index))

console.log('== 错误路径 ==')
const bad = makeCtx(TEMP)
const badMod = await import(pathToFileURL(PLUGIN).href + '?v=2')
badMod.apply(bad.ctx, { binary: join(TEMP, 'no-such-binary', exe) })
let message = ''
try { await bad.tools.get('dk_check').execute({}, { agent: {}, signal: undefined }) } catch (e) { message = String(e.message) }
ok('二进制不存在时报可读错误', /无法启动|dk serve 退出/.test(message), message.slice(0, 120))
ok('并提示改 binary 配置', /binary/.test(message))

disposeAll({ effects }, second, up, bad)
console.log('')
console.log('结果：' + pass + ' 通过 / ' + fail + ' 失败')
process.exit(fail === 0 ? 0 : 1)
