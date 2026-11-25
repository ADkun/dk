import { test } from 'node:test'
import assert from 'node:assert/strict'
import { apply } from '../index.js'

/** 最小 ctx：只实现 apply() 真正用到的三样（effect / inject / tools.register）。 */
function harness({ prompt = true, ready = 'now' } = {}) {
  const sections = []
  const disposers = []
  let late = null
  const childCtx = { systemPrompt: { section(s) { sections.push(s) } } }
  const ctx = {
    // 旧写法读的就是这个：服务没就绪时它是 undefined。
    get() { return ready === 'now' ? childCtx.systemPrompt : undefined },
    effect(fn) { const d = fn(); if (typeof d === 'function') disposers.push(d); return () => {} },
    inject(deps, cb) {
      assert.deepEqual(deps, ['systemPrompt'], '只等 systemPrompt 这一个服务')
      if (ready === 'now') cb(childCtx)
      else late = () => cb(childCtx)
      return { dispose() { disposers.push('fiber') } }
    },
    tools: { register() { return () => {} } },
  }
  apply(ctx, { prompt })
  return { sections, disposers, arrive: () => late && late() }
}

test('注册的 section 名、位次与正文', () => {
  const h = harness()
  assert.equal(h.sections.length, 1)
  assert.equal(h.sections[0].name, 'dk:index')
  assert.equal(h.sections[0].order, 11500)
  assert.match(h.sections[0].text, /dk_map/)
  assert.match(h.sections[0].text, /dk_put/)
})

test('末尾带跨库读法：dk -C 只读查看，且不切过去写卡', () => {
  const h = harness()
  const text = h.sections[0].text
  // 命令必须带任务描述：不带只回目录树，模型跑一次拿不到卡片就会退回搜文件。
  assert.match(text, /dk -C <目录> map <任务描述>/)
  assert.match(text, /不要切过去写卡/)
  // 别家索引不全是最常见的态：只在「读不到」时才说 `--allow-partial`，且必须先于「补卡」。
  assert.match(text, /--allow-partial/)
  assert.match(text, /别去补卡/)
  // A5：只描述动作，不能出现任何 harness 工具名。
  assert.ok(!/read|write|edit|ask_user/.test(text.replace(/dk_\w+/g, '')), text)
})

test('systemPrompt 晚到也照样注入（旧的一次性探测在这里会漏）', () => {
  const h = harness({ ready: 'later' })
  assert.equal(h.sections.length, 0, 'apply 那一刻服务还没就绪')
  h.arrive()
  assert.equal(h.sections.length, 1, '服务就绪后必须补上，不能永久跳过')
})

test('prompt:false 既不注册也不等依赖', () => {
  const h = harness({ prompt: false })
  assert.equal(h.sections.length, 0)
  assert.ok(!h.disposers.includes('fiber'), '不该为一段不注入的提示词留 fiber')
})

test('卸载时拆掉注入 fiber', () => {
  const h = harness()
  const last = h.disposers.at(-1)
  assert.equal(typeof last, 'function')
  last()
  assert.ok(h.disposers.includes('fiber'))
})
