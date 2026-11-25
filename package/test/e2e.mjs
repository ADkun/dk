/**
 * fake-ctx 端到端：不启动 DSH，直接按插件契约模拟 ctx，跑完插件壳的真实链路。
 * 断言五件事：①Config 探测失败不炸 ②六个工具按原生名注册 ③execute 转发到 dk serve 并拿到结果
 * ④系统提示词只加一段、order/文案符合 SPEC §6 ⑤换 root 会重启子进程。
 *
 * 前置：cargo build（debug 即可）。二进制可用环境变量 DK_BIN 指定。
 * 运行：node package/test/e2e.mjs（它是端到端脚本，不是 *.test.mjs，node --test 不会自动收它）
 */
import { spawn } from 'node:child_process'
import { mkdirSync, writeFileSync, existsSync, readFileSync, rmSync } from 'node:fs'
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

/**
 * 三处工具声明（serve 的 arg_keys、mcp.rs 的 tool_specs、package/index.js 的 SPECS）必须同步，
 * 而过去「同步」只有人眼守着：实测 dk_map 两侧描述差了 38 个字，谁也没发现。这里直接对拍
 * 两份运行时字符串——壳里的描述就是模型读到的那段话，它漂了没人会知道。
 */
function mcpToolDescriptions() {
  return new Promise((resolve, reject) => {
    const child = spawn(BIN, ['mcp'], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
    const timer = setTimeout(() => { child.kill(); reject(new Error('dk mcp 无响应')) }, 15000)
    let buf = ''
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', chunk => {
      buf += chunk
      let at
      while ((at = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, at).trim()
        buf = buf.slice(at + 1)
        if (!line) continue
        let msg
        try { msg = JSON.parse(line) } catch { continue }
        if (Array.isArray(msg.result?.tools)) {
          clearTimeout(timer)
          child.kill()
          resolve(new Map(msg.result.tools.map(t => [t.name, t.description || ''])))
        }
      }
    })
    child.on('error', error => { clearTimeout(timer); reject(error) })
    const send = m => child.stdin.write(JSON.stringify(m) + '\n')
    send({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'e2e', version: '0' } } })
    setTimeout(() => {
      send({ jsonrpc: '2.0', method: 'notifications/initialized' })
      send({ jsonrpc: '2.0', id: 2, method: 'tools/list' })
    }, 200)
  })
}

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

ok('dk_map 的描述点名「只覆盖当前仓库」', /只覆盖当前仓库/.test(tools.get('dk_map').description || ''))
// 不带任务描述的 dk -C <目录> map 只回目录树：命令写对了参数没写，等于白跑一趟。
ok('dk_map 的跨库命令带 <任务描述>', /dk -C <目录> map <任务描述>/.test(tools.get('dk_map').description || ''))

console.log('== 三处声明不许漂：插件描述 vs dk mcp ==')
const mcpDesc = await mcpToolDescriptions()
const drift = EXPECT.filter(n => (tools.get(n)?.description || '') !== (mcpDesc.get(n) || ''))
ok('六个工具的描述两侧逐字相同', drift.length === 0, drift.length ? '漂移：' + drift.join(',') : '6/6')

console.log('== 系统提示词 ==')
ok('只加一段', sections.length === 1, String(sections.length))
ok('order = 11500', sections[0]?.order === 11500, String(sections[0]?.order))
ok('不含 harness 工具名（read/write/edit/ask_user）',
  !/read|write|edit|ask_user/.test(sections[0]?.text.replace(/dk_\w+/g, '') || 'x'))
ok('文案里点名 dk_map / dk_put', /dk_map/.test(sections[0]?.text || '') && /dk_put/.test(sections[0]?.text || ''))
ok('文案末尾带跨库读法（dk -C 只读、不切过去写卡）',
  /dk -C <目录> map/.test(sections[0]?.text || '') && /不要切过去写卡/.test(sections[0]?.text || ''))
ok('提示词里的跨库命令也带 <任务描述>', /map <任务描述>/.test(sections[0]?.text || ''))
// PATH 上的 dk 可能比插件实跑的旧（本机实测 PATH 上是 0.2.7）：照抄提示里的命令会落到旧语义，
// 而旧的 next 还在教「先 dk_sweep 补卡」——命令成功退出，看不出问题（审查 P2-1）。
const promptText = sections[0]?.text || ''
ok('提示词点名实跑的二进制（不是 PATH 上的旧 dk）', promptText.includes(BIN.replace(/\\/g, '/')), promptText.slice(-70))
// 反斜杠路径粘进 Git Bash 会被吃成转义（实测 exit 127）：提示里出现的必须是能直接粘的形态。
ok('提示里的二进制路径用正斜杠（bash / pwsh 都能直接粘）', !/[A-Za-z]:\\/.test(promptText), promptText.slice(-70))
ok('提示词不用输出里不存在的状态字符串（判别字段是 incomplete）', promptText.includes('incomplete') && !promptText.includes('索引不全'), promptText.slice(-140))

console.log('== 第三层：/dk SKILL（按需载入，命令必须与提示词同形）==')
// 三层里只有 SKILL 是「用户明确要治理」时才到的位置，它写错等于强提醒写错。
const skill = readFileSync(join(SELF, 'skills', 'dk', 'SKILL.md'), 'utf8')
ok('SKILL 带同一条跨库读法', /dk -C <目录> map <任务描述>/.test(skill) && /不要切过去写卡/.test(skill))
ok('SKILL 里建库前先问人（别在别人的仓库里 dk_init）', /先问用户要不要/.test(skill))
// A5 管的是所有会进上下文的文案，不只是提示词：六条工具描述同样每次都在。
const a5 = (s) => !/read|write|edit|ask_user/.test(String(s).replace(/dk_\w+/g, ''))
ok('A5：六条工具描述都不含 harness 工具名', EXPECT.every(n => a5(tools.get(n).description || '')),
  'dk:' + EXPECT.filter(n => !a5(tools.get(n).description || '')).join(','))
ok('A5：SKILL 也不含 harness 工具名（扣掉 dk_* 后）', a5(skill))

const exec = { agent: {}, signal: undefined }

console.log('== 只读链路（本仓库）==')
const check = await tools.get('dk_check').execute({}, exec)
ok('dk_check 返回对象', typeof check === 'object' && check !== null, JSON.stringify(check).slice(0, 120))

const map = await tools.get('dk_map').execute({ task: '检索怎么算相关度' }, exec)
// 原来是 `Array.isArray(cards) || Array.isArray(dirs) || status`——任何不抛错的返回都为真，
// 连 status:"uninitialized" 都算通过，于是「命中并给出卡片行」这句话其实从没被测过。
ok('dk_map 有命中（真出卡片，不是只回一个状态）', Array.isArray(map.cards) && map.cards.length > 0, JSON.stringify(map).slice(0, 200))
const rendered = tools.get('dk_map').output.render({}, map)
ok('output.render 产出卡片行（带指纹与 F:）',
  rendered.length === 1 && rendered[0].type === 'text' && /@[0-9a-f]{8}: F:/.test(rendered[0].text),
  (rendered[0]?.text || '').slice(0, 80))

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
// 未知参数别只纠错：想够别的仓库的人最容易在这里撞墙（root / repo / cwd）。
ok('未知参数 root 的报错给出路（要看别的仓库用 -C）',
  /dk -C <目录> map <任务描述>/.test(await errOf({ task: 'bm25', root: '../x' })))

console.log('== 库外路径不许「成功地空着回去」（审查 S3）==')
// dir:"../别处" 原来回 count:0（像「这目录没卡」），paths:["../别处/a.rs"] 原来回
// state:"uncarded"（等于断言「这文件没建卡」）——都是假话，而 ../ 恰是想够别人仓库时的第一反应。
const outOf = async (call) => {
  try { return 'OK:' + JSON.stringify(await call()).slice(0, 100) } catch (e) { return String(e?.message || e) }
}
const mapTool = tools.get('dk_map'), getTool = tools.get('dk_get')
ok('dir 越界（..）→ 报错', /只认本仓库相对路径/.test(await outOf(() => mapTool.execute({ dir: '../dsh-other' }, exec))))
ok('dir 越界（绝对路径）→ 报错', /只认本仓库相对路径/.test(await outOf(() => mapTool.execute({ dir: 'D:/dsh' }, exec))))
ok('paths 越界 → 报错', /只认本仓库相对路径/.test(await outOf(() => mapTool.execute({ paths: ['../other/a.rs'] }, exec))))
ok('paths 绝对路径 → 报错', /只认本仓库相对路径/.test(await outOf(() => mapTool.execute({ paths: ['D:/dsh/a.rs'] }, exec))))
// "." 表示整库，只对 dir 成立；当文件用就成了「对空路径断言这文件没建卡」。
ok('paths:"." → 报错并指路 dir', /整库用 dir/.test(await outOf(() => mapTool.execute({ paths: ['.'] }, exec))))
ok('dk_get "." → 报错并指路 dir', /整库用 dir/.test(await outOf(() => getTool.execute({ paths: ['.'] }, exec))))
// 带行号是模型最常见的输入形态：报错要说「别带行号」，不能说成「库外路径」。
ok('dk_get 带行号 → 报错点明行号，不谎称库外', /不要带行号/.test(await outOf(() => getTool.execute({ paths: ['SPEC.md:12'] }, exec))))
ok('dk_get 越界 → 报错', /只认本仓库相对路径/.test(await outOf(() => getTool.execute({ paths: ['../other/a.rs'] }, exec))))
const whole = await mapTool.execute({ dir: '.' }, exec)
ok('dir:"." 给整个仓库，不是「匹配不到」', Array.isArray(whole.cards) && whole.cards.length > 10, String(whole.cards?.length))
// 「成功地空着回去」不止越界一种形态：目录、拼错的文件名、不存在的 dir 同样是假话
// （审查 P1-2 / P2-2：get crates 回 state:"uncarded"＝对目录断言「这文件没建卡」）。
ok('dk_get 收目录 → 报错并指路 dir', /这是目录/.test(await outOf(() => getTool.execute({ paths: ['crates'] }, exec))))
ok('dk_map 的 paths 收目录 → 报错', /这是目录/.test(await outOf(() => mapTool.execute({ paths: ['crates'] }, exec))))
ok('拼错的文件名 → 报错，不谎称未建卡', /磁盘上没有它/.test(await outOf(() => getTool.execute({ paths: ['src/alhpa.rs'] }, exec))))
ok('dir 收不存在的目录 → 报错，不回 count:0', /只认库内已存在的目录/.test(await outOf(() => mapTool.execute({ dir: 'nope' }, exec))))

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
// 没索引的目录最容易被照着 hint 在错的地方建出第二份 .dk/：hint 必须讲清 -C 指哪儿，
// 并且不许直接叫人 dk init（要不要建由那个仓库的人决定）。
const sub = cli(['map', '-C', join(SELF, 'crates')])
ok('没索引的目录：hint 讲清 -C 要指仓库根，不复读「执行 dk init」',
  /-C 要指向仓库根/.test(sub.out) && !/执行 dk init/.test(sub.out),
  (sub.out.match(/"hint": "[^"]{0,40}/) || [sub.err.trim().slice(0, 60)])[0])

// 同一个事实不能两个壳两套答案：dk_map 会说「这里没有索引」，而 dk check / dk sweep
// 原来在同一个目录回「受管 0 文件 / 覆盖率 100% | 一致」——最坏的一种假绿。
const noIdxGet = cli(['get', '-C', join(SELF, 'crates'), 'SPEC.md'])
const noIdxSweep = cli(['sweep', '-C', join(SELF, 'crates')])
const noIdxCheck = cli(['check', '-C', join(SELF, 'crates')])
ok('没索引：get 也给同一句 hint', /-C 要指向仓库根/.test(noIdxGet.out), noIdxGet.err.trim().slice(0, 60))
ok('没索引：sweep 不许回「还差 0」', /-C 要指向仓库根/.test(noIdxSweep.err) && !/还差 0/.test(noIdxSweep.out), (noIdxSweep.out + noIdxSweep.err).trim().slice(0, 60))
ok('没索引：check 不许回「一致」', /-C 要指向仓库根/.test(noIdxCheck.err) && !/覆盖率/.test(noIdxCheck.out), (noIdxCheck.out + noIdxCheck.err).trim().slice(0, 60))

// 目录不存在 ≠ 没有索引：两句提示对应两种下一步（改 -C 还是去问那个仓库的人）。
const noDir = cli(['check', '-C', join(SELF, 'no-such-dir')])
const noDirMap = cli(['map', '-C', join(SELF, 'no-such-dir'), 'x'])
ok('目录不存在：check 与 map 同答「工作目录不存在」',
  /工作目录不存在/.test(noDir.err) && /工作目录不存在/.test(noDirMap.err),
  (noDir.err || noDir.out).trim().slice(0, 60))

// 跑 scripts/ 下的零依赖脚本：与 CLI 同样的道理，脚本自己也有假成功要测。
const node = (args) => {
  try {
    return { out: execFileSync(process.execPath, args, { cwd: SELF, encoding: 'utf8' }), err: '' }
  } catch (e) {
    return { out: String(e.stdout || ''), err: String(e.stderr || e.message) }
  }
}

console.log('== 发版脚本：正文只有一个来源，同一版本的多行都要取上 ==')
// v0.3.5 在 §12 里分「功能」与「发布整理」两行：只取一行等于把另一半变更从 Release 里删掉，
// 而 Release 正文又是脚本生成的，删了没人会当场发现（首发那次就漏了「发布整理」那半）。
const notesV = node(['scripts/release-notes.mjs', 'v0.3.5'])
ok('Release 正文把同一版本的多行都取上', /^### v0\.3\.5：/.test(notesV.out) && /^### v0\.3\.5 发布整理：/m.test(notesV.out),
  (notesV.out.match(/^### v0\.3\.5[^—]{0,12}/gm) || [notesV.err]).join(' / ').slice(0, 80))
ok('正文尾部要带安装段（下载文件名带版本号）', /\*\*安装\*\*/.test(notesV.out) && notesV.out.includes('dk-v0.3.5-x86_64-pc-windows-msvc.zip'))
// §12 的一格是表格单元，里面没有换行：原样拼进正文就是一面墙（v0.3.5 首发就是这样，用户截图反馈）。
// 判据钉在「每项一行」与「最长行」上——换行是给人读的，退化回去要当场看见。
const bullets = (notesV.out.match(/^- [①②③④⑤⑥⑦⑧⑨⑩]/gm) || []).length
ok('正文按圈号切成条目（一条变更一行，不再是一面墙）', bullets >= 10, '条目数 ' + bullets)
const longest = Math.max(...notesV.out.split('\n').map((l) => l.length))
ok('正文里没有超长行（最长 500 字以内）', longest < 500, '最长行 ' + longest + ' 字')
const notesMiss = node(['scripts/release-notes.mjs', 'v0.0.0'])
ok('§12 没有那一版 → 退出非 0，不产出空正文', /没有 v0\.0\.0/.test(notesMiss.err) && notesMiss.out.trim() === '',
  notesMiss.err.trim().slice(0, 60))

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

// incomplete 是跨库最常见的态（别人的索引很少刚好 100%）。这一行的出路必须第一句
// 就是「只读」，并且命令要能照着敲：原来写的是 MCP 形状的 allow_partial:true，CLI 照抄跑不通。
ok('incomplete 的 next 先给只读出路，不推人去补别人的卡',
  /--allow-partial/.test(half.next || '') && /别在别人仓库里补/.test(half.next || ''),
  String(half.next || '').slice(0, 120))
const cliIncomplete = cli(['map', '-C', TEMP, 'alpha 常量'])
ok('CLI 照 next 敲真能过（--allow-partial 就在文案里）',
  /--allow-partial/.test(cliIncomplete.out) && /incomplete/.test(cliIncomplete.out),
  (cliIncomplete.out + cliIncomplete.err).trim().slice(0, 100))
const cliPartial = cli(['map', '-C', TEMP, 'alpha 常量', '--allow-partial'])
ok('加上 --allow-partial 立刻出卡（不是只换一句话）', /@[0-9a-f]{8}:/.test(cliPartial.out), cliPartial.err.trim().slice(0, 80))

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
