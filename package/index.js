/**
 * dsh-dk — 把 dk 代码索引接进 DSH（SPEC §9.3）。
 *
 * 只做三件事：内置拉起 dk serve、把六个工具注册成原生名、追加一句系统提示词。
 * 不注册任何钩子，没有业务逻辑，只有转发——所有行为都来自 dk serve 的 NDJSON。
 *
 * 零 @deepseek-ai 运行时依赖：Config 的 schemastery 描述符按需探测，解析不到只是
 * 没有设置页表单，绝不拖垮插件本身（与 dsh-git-bash 同一套写法）。
 */
import { spawn } from 'node:child_process'
import { existsSync, readdirSync, statSync } from 'node:fs'
import { createRequire } from 'node:module'
import { homedir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

export const name = 'dsh-dk'
export const inject = ['tools']

const DEFAULTS = { root: '', binary: 'dk', prompt: true, callTimeoutMs: 120000 }

/** 唯一常驻的自动提醒。只描述动作，不出现任何 harness 工具名（SPEC 公理 A5）。 */
const GUIDANCE =
  '若这个仓库有 .dk/map.txt（dk 代码索引：一行一文件，写职责 / 关系 / 契约 / 约束），'
  + '改代码前先 dk_map（给任务描述）或 dk_get（给路径），不要搜完就动手；'
  + '改完用 dk_put 把改过的文件写回（F、R 必写，A、S 没有就省）；'
  + '卡片与源码对不上（stale）就先重读源码再重写。'
  + '没有索引时不要擅自建，先问用户。'

// --- Config -----------------------------------------------------------------

function schemaBases() {
  const bases = [import.meta.url]
  let home = ''
  try { home = process.env.DSH_HOME || join(homedir(), '.dsh') } catch { home = '' }
  return { bases, home }
}

function loadSchema() {
  const { bases, home } = schemaBases()
  const dirs = []
  if (process.env.DSH_PROFILE_DIR) dirs.push(process.env.DSH_PROFILE_DIR)
  if (home) {
    try {
      for (const entry of readdirSync(join(home, 'profiles'))) {
        const dir = join(home, 'profiles', entry)
        try { if (statSync(join(dir, 'package.json')).isFile()) dirs.push(dir) } catch { /* 跳过 */ }
      }
    } catch { /* 没有 profiles 目录 */ }
  }
  if (process.env.APPDATA) dirs.push(join(process.env.APPDATA, 'npm', 'node_modules', '@deepseek-ai', 'dsh'))
  for (const dir of dirs) {
    try { bases.push(pathToFileURL(join(dir, 'package.json')).href) } catch { /* 忽略 */ }
  }
  for (const base of bases) {
    try {
      const require = createRequire(base)
      const z = require('@deepseek-ai/schemastery')
      if (!z || typeof z.object !== 'function') continue
      return z.object({
        root: z.string().default(DEFAULTS.root).volatile(),
        binary: z.string().default(DEFAULTS.binary).volatile(),
        prompt: z.boolean().default(DEFAULTS.prompt).volatile(),
        callTimeoutMs: z.number().default(DEFAULTS.callTimeoutMs).volatile(),
      })
    } catch { /* 试下一个基准 */ }
  }
  return undefined
}

export const Config = loadSchema()

function resolveConfig(config) {
  const raw = config && typeof config === 'object' ? config : {}
  const str = (v, d) => (typeof v === 'string' ? v.trim() : d)
  const parsed = Number(raw.callTimeoutMs)
  return {
    root: str(raw.root, DEFAULTS.root),
    binary: str(raw.binary, DEFAULTS.binary) || DEFAULTS.binary,
    prompt: typeof raw.prompt === 'boolean' ? raw.prompt : DEFAULTS.prompt,
    callTimeoutMs: Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : DEFAULTS.callTimeoutMs,
  }
}

// --- 仓库根与可执行文件 -------------------------------------------------------

/**
 * 会话目录不一定是仓库根：人 cd 进 src/ 干活是平常事。dk 自己不上溯（CLI 里 -C 是
 * 什么就是什么），所以「会话目录 → 仓库根」这一步归插件管：向上找 .dk/，其次 .git/，
 * 都没有就用原目录（此时 dk 回 uninitialized，人再决定要不要 init）。
 */
function repoRoot(start) {
  let dir = ''
  try { dir = resolve(start) } catch { return start }
  let probe = dir
  let git = ''
  for (;;) {
    if (existsSync(join(probe, '.dk'))) return probe
    if (!git && existsSync(join(probe, '.git'))) git = probe
    const up = dirname(probe)
    if (up === probe) break
    probe = up
  }
  return git || dir
}

/**
 * 默认二进制：优先源码构建产物（本仓库自带的 target/{release,debug}），否则走 PATH 上的
 * dk。写死绝对路径的配置换台机器就 ENOENT（审查 S3）。显式配了 binary 就照用。
 */
function resolveBinary(pref) {
  if (pref && pref !== DEFAULTS.binary) return pref
  let here = ''
  try { here = fileURLToPath(new URL('.', import.meta.url)) } catch { here = '' }
  if (here) {
    const name = process.platform === 'win32' ? 'dk.exe' : 'dk'
    for (const profile of ['release', 'debug']) {
      const candidate = join(here, '..', 'target', profile, name)
      if (existsSync(candidate)) return candidate
    }
  }
  return DEFAULTS.binary
}

// --- dk serve 客户端 ---------------------------------------------------------

/**
 * 一个 root 一个常驻进程。进程死了不阻塞调用方，下一次调用重新拉起；
 * root 变了（会话换了目录）先杀旧进程再起新的：索引只对它的仓库有意义。
 *
 * 每次 spawn 自带一份 pending：旧进程的 exit 是异步到达的，若共用一张表，它会把新进程
 * 的请求一起 reject、把 this.child 抹成 null（审查 S1：换 root 后那次调用必失败，且每次
 * 切换泄漏一个 dk serve）。
 */
class Server {
  constructor(binary, timeoutMs) {
    this.binary = binary
    this.timeoutMs = timeoutMs
    this.root = ''
    this.child = null
    this.seq = 0
    this.pending = new Map()
    this.buffer = ''
  }

  ensure(root) {
    if (this.child && this.root === root) return
    this.stop()
    this.root = root
    this.buffer = ''
    const child = spawn(this.binary, ['serve', '-C', root], {
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    })
    const mine = new Map()
    this.pending = mine
    this.child = child
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', chunk => this.onData(child, chunk))
    const errs = []
    child.stderr.on('data', chunk => {
      errs.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk)))
      if (errs.length > 64) errs.shift()
    })
    // 非 UTF-8 程序（cmd.exe 之类）解出来是满屏 U+FFFD，退回单字节解码至少能看（审查 G7）。
    const tail = () => {
      const buf = Buffer.concat(errs)
      const utf8 = buf.toString('utf8')
      return (utf8.includes('\uFFFD') ? buf.toString('latin1') : utf8).trim().slice(-2000)
    }
    child.on('error', error => {
      this.fail(child, mine, '无法启动 ' + this.binary + '：'
        + (error && error.message ? error.message : String(error))
        + '（把 binary 配成 dk 可执行文件的绝对路径，或先 cargo build --release）')
    })
    child.on('exit', code => {
      const detail = tail()
      this.fail(child, mine, 'dk serve 退出（code ' + code + '）' + (detail ? '：' + detail : ''))
    })
  }

  onData(child, chunk) {
    if (child !== this.child) return
    this.buffer += chunk
    let index = this.buffer.indexOf('\n')
    while (index >= 0) {
      const line = this.buffer.slice(0, index).trim()
      this.buffer = this.buffer.slice(index + 1)
      if (line) this.onLine(child, line)
      index = this.buffer.indexOf('\n')
    }
  }

  onLine(child, line) {
    let message
    try { message = JSON.parse(line) } catch { return }
    const entry = child === this.child ? this.pending.get(message.id) : undefined
    if (!entry) return
    if (message.ok) entry.resolve(message.result)
    else entry.reject(new Error(message.error || 'dk 报错（无消息）'))
  }

  /** 只收自己那一批：新进程的 pending 不归旧进程的 exit 管。 */
  fail(child, mine, reason) {
    if (this.child === child) {
      this.child = null
      this.root = ''
    }
    const entries = [...mine.values()]
    mine.clear()
    for (const entry of entries) entry.reject(new Error(reason))
  }

  stop() {
    const child = this.child
    this.child = null
    this.root = ''
    if (!child) return
    try { child.stdin.end() } catch { /* 已关 */ }
    try { child.kill() } catch { /* 已退出 */ }
  }

  call(tool, args, signal) {
    const child = this.child
    if (!child) throw new Error('dk serve 尚未启动')
    if (signal && signal.aborted) return Promise.reject(new Error('已取消'))
    const id = process.pid + '-' + (++this.seq)
    return new Promise((resolve, reject) => {
      const entry = { timer: null, off: null }
      let closed = false
      const settle = (fn, value) => {
        if (closed) return
        closed = true
        this.pending.delete(id)
        if (entry.timer) clearTimeout(entry.timer)
        if (entry.off) entry.off()
        fn(value)
      }
      entry.resolve = value => settle(resolve, value)
      entry.reject = error => settle(reject, error)
      entry.timer = setTimeout(() => {
        settle(reject, new Error(tool + ' 超时（' + this.timeoutMs + ' ms）'))
      }, this.timeoutMs)
      if (typeof entry.timer.unref === 'function') entry.timer.unref()
      if (signal && typeof signal.addEventListener === 'function') {
        // 飞行中的调用也要能被取消，否则中止后仍要等满超时（审查 G6）。
        const onAbort = () => settle(reject, new Error('已取消'))
        signal.addEventListener('abort', onAbort, { once: true })
        entry.off = () => { try { signal.removeEventListener('abort', onAbort) } catch { /* 忽略 */ } }
      }
      this.pending.set(id, entry)
      child.stdin.write(JSON.stringify({ id, tool, args: args || {} }) + '\n')
    })
  }
}

// --- 工具声明 ---------------------------------------------------------------

// 参数名与 crates/dk-core/src/serve.rs 的 handle 逐一对齐；对外的 MCP 版在
// crates/dk-core/src/mcp.rs。改一处就要改另两处；六工具名的比对在
// package/test/e2e.mjs（脚本，不是 cargo test 覆盖的部分）与 package/test/prompt.test.mjs。
const SPECS = [
  {
    name: 'dk_init',
    concurrency: false,
    description: '初始化本仓库的代码索引（建 .dk/ 与 .dkignore、写 AGENTS.md 受管块）。'
      + '人触发的动作，不写卡片。',
    parameters: {
      type: 'object',
      additionalProperties: false,
      properties: {
        agents: { type: 'boolean', description: '是否写入 AGENTS.md 受管块（默认 true）' },
      },
    },
  },
  {
    name: 'dk_map',
    concurrency: true,
    description: '查代码索引：给 task 拿最相关的几张卡片（每张含 职责/关系/契约/约束），也可给 paths 或 dir 直接取；'
      + '不给参数时给目录树。索引没建完时默认拒绝给残缺结果。'
      + 'confidence 只说词面是怎么对上的（surface/fallback/none），不是「答案在里面」；'
      + 'matched 里 hit 是首张卡踩中的查询词、of 是分母（只数本仓出现过、且不是过半卡片都有的词），absent 是本仓一张卡都没有的词——'
      + 'hit 少而 absent 多说明该换词或给 paths，不是问法不好。of=0 时 hit 必为空（分母里没有有判别力的词），看 weak_n/absent_n 才知道是哪一类。'
      + '命中不等于答案。task 传空字符串、paths 传空数组都报错——成功但空的结果等于没问。',
    parameters: {
      type: 'object',
      additionalProperties: false,
      properties: {
        task: { type: 'string', description: '自然语言任务描述，例如「修会话恢复时的锁竞争」' },
        // 只声明数组：anyOf 不在 DSH 的 schema 子集里，走 defineTool 会 UNSUPPORTED_SCHEMA（审查 G8）。
        // dk 侧仍宽容接受单个字符串，但声明只有一种形态，模型才不会猜。
        hints: { type: 'array', items: { type: 'string' }, description: '补充关键词，有一条就放一条' },
        paths: { type: 'array', items: { type: 'string' }, description: '直接按路径取卡，不做检索' },
        dir: { type: 'string', description: '取某目录下全部卡' },
        level: {
          type: 'string',
          enum: ['l0', 'l1', 'l2'],
          description: '披露等级：l0 只给目录、l1 给路径加一句话、l2 给整卡（默认）',
        },
        allow_partial: { type: 'boolean', description: '索引未建完仍给结果，结果会标注不含哪些文件' },
        k: { type: 'integer', description: '卡片张数（不含 R 扩散邻居），默认 6' },
      },
    },
  },
  {
    name: 'dk_get',
    concurrency: true,
    description: '按路径取整卡，并顺手校验卡片记录的源码哈希是否还新鲜；同时给出这些卡在关系图里的邻居'
      + '（只给路径与一句话）。不返回源码。',
    parameters: {
      type: 'object',
      additionalProperties: false,
      properties: {
        paths: { type: 'array', items: { type: 'string' }, description: '仓库相对路径' },
      },
      required: ['paths'],
    },
  },
  {
    name: 'dk_put',
    concurrency: false,
    description: '写回卡片：传了哪个字段就覆盖哪个，没传的保留。源码哈希由工具自己现算，不要手填。一次调用写完这批全部文件。',
    parameters: {
      type: 'object',
      additionalProperties: false,
      properties: {
        entries: {
          type: 'array',
          items: {
            type: 'object',
            additionalProperties: false,
            properties: {
              path: { type: 'string', description: '仓库相对路径' },
              F: { type: 'string', description: '一句核心职责' },
              R: { type: 'string', description: '必须一起考虑的强关系，逗号分隔 code:<路径>；无则省' },
              A: { type: 'string', description: '稳定对外契约：导出签名、CLI 参数、协议字段、表结构；无则省' },
              S: { type: 'string', description: '高熵约束：不写就会改错的坑；能从其它字段推出就不写' },
              tags: { type: 'string', description: '可选标签，原样留存不解析' },
              skip: { type: 'boolean', description: '这个文件不值得建卡：删掉它的卡并记入 .dkignore 提议区' },
            },
            required: ['path'],
          },
        },
        drop_bad: { type: 'boolean', description: '顺手清掉索引里无法解析的历史坏行' },
      },
      required: ['entries'],
    },
  },
  {
    name: 'dk_sweep',
    concurrency: true,
    description: '列出还没建卡的文件（只给路径、字节数、行数、机器特征，不给源码）。读文件、判断值不值得建卡都是你自己的事；'
      + '不值得就用 dk_put 传 skip。重跑即续跑。',
    parameters: {
      type: 'object',
      additionalProperties: false,
      properties: { limit: { type: 'integer', description: '本批最多几个文件，默认 10' } },
    },
  },
  {
    name: 'dk_check',
    concurrency: true,
    description: '全量体检：受管文件数、有卡数、覆盖率、过期、失联、未建卡、坏关系、坏行、重复卡。',
    parameters: { type: 'object', additionalProperties: false, properties: {} },
  },
]

const OUTPUT = {
  schema: { type: 'object' },
  render(_args, value) {
    return [{ type: 'text', text: JSON.stringify(value, null, 2) }]
  },
}

// --- 插件入口 ---------------------------------------------------------------

async function resolveRoot(exec, cfg, ctx) {
  if (cfg.root) return cfg.root
  // workingDirectory 在调用期现取：本插件 apply 时它可能还没被别的插件 provide（审查 G3）。
  if (exec && exec.agent) {
    const wd = typeof ctx.get === 'function' ? ctx.get('workingDirectory') : undefined
    if (wd && typeof wd.ensure === 'function') {
      try {
        const dir = await wd.ensure(exec.agent, exec.signal)
        if (dir) return repoRoot(dir)
      } catch { /* 退回进程目录 */ }
    }
  }
  try { return repoRoot(process.cwd()) } catch { return '.' }
}

export function apply(ctx, config) {
  const cfg = resolveConfig(config)
  const server = new Server(resolveBinary(cfg.binary), cfg.callTimeoutMs)

  ctx.effect(() => () => server.stop(), 'dsh-dk: stop dk serve')

  for (const spec of SPECS) {
    const definition = {
      name: spec.name,
      description: spec.description,
      parameters: spec.parameters,
      output: OUTPUT,
      isConcurrencySafe: () => spec.concurrency,
      async execute(args, exec) {
        if (exec && exec.signal && exec.signal.aborted) throw new Error('已取消')
        const root = await resolveRoot(exec, cfg, ctx)
        server.ensure(root)
        return server.call(spec.name, args || {}, exec && exec.signal)
      },
    }
    ctx.effect(() => {
      const dispose = ctx.tools.register(definition)
      return () => { try { dispose() } catch { /* 已卸载 */ } }
    }, 'dsh-dk: register ' + spec.name)
  }

  // 那一句常驻提醒必须等 systemPrompt 就绪再注册。早先是 `ctx.get('systemPrompt')` 探一次：
  // apply 若跑在 dsh-system-prompt 之前，那一刻是 undefined，于是静默跳过、此后永不重试 ——
  // 表现就是「有时注入、有时整段提示词里没有」。对照 dsh-adg-ptc 的 skill-inject：它用
  // `export const inject = ['systemPrompt']` 等依赖；我们是 host 插件、又不该要求每个 profile
  // 都有 systemPrompt（没有它工具也照用），所以用 ctx.inject 惰性等，服务一到就注册。
  if (cfg.prompt) {
    ctx.effect(() => {
      let fiber
      try {
        fiber = ctx.inject(['systemPrompt'], (childCtx) => {
          const prompt = childCtx.systemPrompt
          if (!prompt || typeof prompt.section !== 'function') return
          // 注册挂在 childCtx 上，随 fiber 一起拆；这里不再单独拆 section。
          prompt.section({ name: 'dk:index', order: 11500, text: GUIDANCE })
        })
      } catch { /* 没有注入能力时，工具照样可用，只是少一句提示 */ }
      return () => { try { if (fiber && typeof fiber.dispose === 'function') fiber.dispose() } catch { /* 已拆 */ } }
    }, 'dsh-dk: prompt section')
  }
}
