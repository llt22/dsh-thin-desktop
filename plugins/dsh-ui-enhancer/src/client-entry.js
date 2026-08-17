import React from 'react'
import { enhanceAssistantNode } from './core.js'

export const inject = ['slots', 'sessions']

function EnhancedAssistantNode(props) {
  const Original = EnhancedAssistantNode.original
  const node = React.useMemo(() => enhanceAssistantNode(props.node), [props.node])
  return React.createElement(
    React.Fragment,
    null,
    React.createElement(Original, { ...props, node }),
  )
}

function registerAssistantRenderer(ctx) {
  let disposeRenderer
  let disposeSubscription

  const register = () => {
    if (disposeRenderer !== undefined) return
    const official = ctx.slots.entries('conversation.chat.node').find((entry) => (
      entry.options.key === 'assistant-step' && (entry.options.priority ?? 0) === 0
    ))
    if (official === undefined) return

    EnhancedAssistantNode.original = official.component
    disposeRenderer = ctx.slots.register({
      name: 'conversation.chat.node',
      key: 'assistant-step',
      priority: -10,
    }, EnhancedAssistantNode)
    disposeSubscription?.()
    disposeSubscription = undefined
  }

  disposeSubscription = ctx.slots.subscribe('conversation.chat.node', register)
  register()

  return () => {
    disposeSubscription?.()
    disposeRenderer?.()
  }
}

function registerEscapeCancellation(ctx) {
  const onKeyDown = (event) => {
    if (event.key !== 'Escape' || event.repeat || event.defaultPrevented) return
    if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return

    const { current, byId } = ctx.sessions.list.getSnapshot()
    if (current === undefined || byId[current]?.running !== true) return
    const session = ctx.sessions.binding(current)?.session
    if (session === undefined) return

    event.preventDefault()
    event.stopImmediatePropagation()
    void session.cancel().then((result) => {
      if (!result.ok) console.error('[dsh-ui-enhancer] failed to cancel generation:', result.error.message)
    }).catch((error) => {
      console.error('[dsh-ui-enhancer] failed to cancel generation:', error)
    })
  }

  document.addEventListener('keydown', onKeyDown, true)
  return () => document.removeEventListener('keydown', onKeyDown, true)
}

export function apply(ctx) {
  ctx.effect(() => registerEscapeCancellation(ctx), 'dsh-ui-enhancer: Escape cancellation')
  ctx.slots.inject('conversation.chat.node', () => registerAssistantRenderer(ctx))
}
