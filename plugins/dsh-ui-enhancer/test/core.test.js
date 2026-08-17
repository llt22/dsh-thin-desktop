import assert from 'node:assert/strict'
import test from 'node:test'
import { enhanceAssistantNode, thinkingText } from '../src/core.js'

test('extracts a complete standalone thinking block', () => {
  assert.equal(thinkingText('  <thinking>\nPlan the change\n</thinking>  '), 'Plan the change')
})

test('rejects empty, mixed, malformed, and attributed blocks', () => {
  assert.equal(thinkingText('<thinking> </thinking>'), undefined)
  assert.equal(thinkingText('prefix <thinking>plan</thinking>'), undefined)
  assert.equal(thinkingText('<thinking>plan</thinking> suffix'), undefined)
  assert.equal(thinkingText('<thinking mode="x">plan</thinking>'), undefined)
  assert.equal(thinkingText('<thinking>unfinished'), undefined)
})

test('maps only matching text blocks to reasoning without mutating the input', () => {
  const node = {
    kind: 'assistant-step',
    data: {
      status: 'settled',
      blocks: [
        { kind: 'reasoning', text: 'native' },
        { kind: 'text', text: '<thinking>compat</thinking>' },
        { kind: 'text', text: 'answer' },
      ],
    },
  }
  const enhanced = enhanceAssistantNode(node)
  assert.notEqual(enhanced, node)
  assert.deepEqual(enhanced.data.blocks, [
    { kind: 'reasoning', text: 'native' },
    { kind: 'reasoning', text: 'compat' },
    { kind: 'text', text: 'answer' },
  ])
  assert.equal(node.data.blocks[1].kind, 'text')
})

test('preserves node identity when no block matches', () => {
  const node = { data: { blocks: [{ kind: 'text', text: 'answer' }] } }
  assert.equal(enhanceAssistantNode(node), node)
})
