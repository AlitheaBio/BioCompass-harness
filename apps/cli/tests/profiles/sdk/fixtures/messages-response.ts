/** Frame one text or tool response from the local Anthropic Messages endpoint. */
export function messagesResponse(content: Record<string, unknown>, stopReason: 'end_turn' | 'max_tokens' | 'tool_use'): string {
  const startBlock = content.type === 'tool_use'
    ? { type: 'tool_use', id: content.id, name: content.name, input: {} }
    : content.type === 'text'
      ? { type: 'text', text: '' }
      : content
  const deltaBlock = content.type === 'tool_use'
    ? { type: 'input_json_delta', partial_json: JSON.stringify(content.input ?? {}) }
    : content.type === 'text'
      ? { type: 'text_delta', text: typeof content.text === 'string' ? content.text : '' }
      : undefined
  return [
    { type: 'message_start', message: { id: 'sdk-smoke-response', model: 'claude-sonnet-4-5', usage: { input_tokens: 3, output_tokens: 0 } } },
    { type: 'content_block_start', index: 0, content_block: startBlock },
    ...deltaBlock === undefined ? [] : [{ type: 'content_block_delta', index: 0, delta: deltaBlock }],
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: stopReason }, usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ].map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')
}
