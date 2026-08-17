const THINKING_BLOCK = /^\s*<thinking>([\s\S]*?)<\/thinking>\s*$/

export function thinkingText(text) {
  if (typeof text !== 'string') return undefined
  const match = THINKING_BLOCK.exec(text)
  if (match === null) return undefined
  const content = match[1].trim()
  return content === '' ? undefined : content
}

export function enhanceAssistantNode(node) {
  const blocks = node?.data?.blocks
  if (!Array.isArray(blocks)) return node

  let changed = false
  const enhanced = blocks.map((block) => {
    if (block?.kind !== 'text') return block
    const text = thinkingText(block.text)
    if (text === undefined) return block
    changed = true
    return { kind: 'reasoning', text }
  })

  if (!changed) return node
  return { ...node, data: { ...node.data, blocks: enhanced } }
}
