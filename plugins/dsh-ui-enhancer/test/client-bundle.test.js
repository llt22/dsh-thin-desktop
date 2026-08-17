import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import vm from 'node:vm'

class FakeElement {
  constructor(tagName = 'div') {
    this.tagName = tagName
    this.dataset = {}
    this.children = []
    this.listeners = new Map()
    this.style = {}
  }

  addEventListener(type, listener) {
    this.listeners.set(type, listener)
  }

  appendChild(child) {
    this.children.push(child)
  }

  closest() {
    return null
  }

  remove() {}

  setAttribute() {}
}

test('the browser bundle waits for and shadows the official renderer, then cancels through the session API', async () => {
  let factory
  const documentListeners = new Map()
  const document = {
    head: new FakeElement('head'),
    body: new FakeElement('body'),
    documentElement: { lang: 'zh-CN' },
    createElement: (tagName) => new FakeElement(tagName),
    addEventListener(type, listener) {
      const listeners = documentListeners.get(type) ?? []
      listeners.push(listener)
      documentListeners.set(type, listeners)
    },
    removeEventListener() {},
    querySelectorAll: () => [],
  }
  const window = {
    __ModuleLoader__: {
      load(definition) {
        assert.equal(definition.id, 'dsh-ui-enhancer')
        factory = definition.factory
      },
    },
  }
  const source = await readFile(new URL('../lib/client.js', import.meta.url), 'utf8')
  vm.runInNewContext(source, {
    console,
    document,
    Element: FakeElement,
    getComputedStyle: () => ({ display: 'block', visibility: 'visible' }),
    window,
  })

  const React = {
    createElement: (type, props, ...children) => ({ type, props: { ...props, children } }),
    useMemo: (factoryFn) => factoryFn(),
  }
  const officialRenderer = () => null
  const slotEntries = []
  let slotSubscriber
  let registered
  let cancelled = 0
  const plugin = factory((id) => {
    if (id === 'react') return React
    throw new Error(`unexpected module: ${id}`)
  })
  const ctx = {
    effect(callback) {
      callback()
    },
    sessions: {
      list: {
        getSnapshot: () => ({ current: 'session-1', byId: { 'session-1': { running: true } } }),
      },
      binding: () => ({
        session: {
          cancel: async () => {
            cancelled += 1
            return { ok: true, value: { accepted: true } }
          },
        },
      }),
    },
    slots: {
      entries: () => slotEntries,
      inject(_name, callback) {
        callback()
      },
      subscribe(_name, callback) {
        slotSubscriber = callback
        return () => {
          slotSubscriber = undefined
        }
      },
      register(options, component) {
        registered = { options, component }
        return () => {}
      },
    },
  }

  plugin.apply(ctx)
  assert.equal(registered, undefined)

  slotEntries.push({
    component: officialRenderer,
    options: { key: 'assistant-step', priority: 0 },
  })
  slotSubscriber()
  assert.equal(registered.options.name, 'conversation.chat.node')
  assert.equal(registered.options.key, 'assistant-step')
  assert.equal(registered.options.priority, -10)

  const rendered = registered.component({
    node: {
      data: { blocks: [{ kind: 'text', text: '<thinking>Use official UI</thinking>' }] },
    },
  })
  const officialElement = rendered.props.children[0]
  assert.equal(officialElement.type, officialRenderer)
  assert.equal(JSON.stringify(officialElement.props.node.data.blocks), JSON.stringify([
    { kind: 'reasoning', text: 'Use official UI' },
  ]))

  const escapeListener = documentListeners.get('keydown').at(-1)
  escapeListener({
    key: 'Escape',
    repeat: false,
    defaultPrevented: false,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    target: new FakeElement(),
    preventDefault() {},
    stopImmediatePropagation() {},
  })
  await Promise.resolve()
  assert.equal(cancelled, 1)
})
