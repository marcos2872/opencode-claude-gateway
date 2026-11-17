import { expect, mock, test } from 'claude-code/testing'

/**
 * The gateways answer is played by a `http.fetch` hook beneath the plugin, and
 * the status line is read back from the `ui.status` hook the plugin raises.
 */
const BASE: {
  provider: string
  windows: Record<string, { percent: number; status: string; resets_at: string | null }>
  credits: { available_micro_cents: number; limit_micro_cents: number | null }
  error: string | null
  status: number | null
  fetched_at: number
} = {
  provider: 'opencode-go',
  windows: {
    rolling: {
      percent: 6.0,
      status: 'ok',
      resets_at: new Date(Date.now() + 2 * 3_600_000 + 14 * 60_000).toISOString(),
    },
    weekly: {
      percent: 27.5,
      status: 'ok',
      resets_at: new Date(Date.now() + 6 * 86_400_000 + 3 * 3_600_000).toISOString(),
    },
    monthly: {
      percent: 41,
      status: 'ok',
      resets_at: new Date(Date.now() + 15 * 86_400_000 + 19 * 3_600_000).toISOString(),
    },
  },
  credits: { available_micro_cents: 364_914_398, limit_micro_cents: null },
  error: null,
  status: null,
  fetched_at: Math.floor(Date.now() / 1000),
}

test('the status line carries all three windows, their resets, and the credits', async ($, on) => {
  const urls: string[] = []
  const lines: (string | undefined)[] = []
  let body = BASE

  on('http.fetch', (_, e) => {
    urls.push(e.url)
    return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify(body) } }
  })
  on('ui.status', (_, e) => {
    lines.push(e.text)
    return { value: undefined }
  })
  on('command.register', () => ({ value: { command: 'agent-usage' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))

  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })

  expect(urls[0]).toBe('http://127.0.0.1:3737/usage')
  const line = lines.at(-1) ?? ''
  // Every window, in the order the status line draws them, each with its own
  // reset, then the credits. No plugin name: the engine prefixes it.
  expect(line).toContain('5h 6% (resets in 2h14m)')
  expect(line).toContain('wk 28% (resets in 6d3h)')
  expect(line).toContain('mo 41% (resets in 15d19h)')
  expect(line).toContain('US$ 3.65')
  expect(line.startsWith('agent-usage:')).toBe(false)
})

test('a quota failure keeps the last value and marks it stale', async ($, on) => {
  const lines: (string | undefined)[] = []
  let body = BASE

  on('http.fetch', () => ({
    value: { status: 200, ok: true, headers: {}, text: JSON.stringify(body) },
  }))
  on('ui.status', (_, e) => {
    lines.push(e.text)
    return { value: undefined }
  })
  on('command.register', () => ({ value: { command: 'agent-usage' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  const clock = mock.clock(on)

  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  expect(lines.at(-1)).not.toContain('(stale)')

  body = { ...BASE, error: 'HTTP 401 unauthorized — run `opencode auth login` again' }
  await clock.advance(60_000)

  expect(lines.at(-1)).toContain('(stale)')
  // A rate-limited window is called out next to its own percentage.
  body = {
    ...BASE,
    windows: { ...BASE.windows, rolling: { percent: 100, status: 'rate-limited', resets_at: null } },
  }
  await clock.advance(60_000)
  expect(lines.at(-1)).toContain('5h 100% limit')
})
