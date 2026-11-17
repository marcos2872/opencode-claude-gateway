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

test('the session token reaches the gateway when the plugin has none', async ($, on) => {
  const headers: (Record<string, string> | undefined)[] = []

  mock.env(on, { ANTHROPIC_AUTH_TOKEN: 'dev-secret' })
  on('http.fetch', (_, e) => {
    headers.push(e.init?.headers)
    return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify(BASE) } }
  })
  on('ui.status', () => ({ value: undefined }))
  on('command.register', () => ({ value: { command: 'agent-usage' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))

  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })

  // The gateway's `auth_token` is what the session sends; a plugin with no
  // `auth_token` option must not go out bare (that was the 401).
  expect(headers[0]?.['x-api-key']).toBe('dev-secret')
})

test(
  'the auth_token option wins over the session token',
  { options: { auth_token: 'dev-secret' } },
  async ($, on) => {
    const headers: (Record<string, string> | undefined)[] = []

    mock.env(on, { ANTHROPIC_AUTH_TOKEN: 'session-token' })
    on('http.fetch', (_, e) => {
      headers.push(e.init?.headers)
      return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify(BASE) } }
    })
    on('ui.status', () => ({ value: undefined }))
    on('command.register', () => ({ value: { command: 'agent-usage' } }))
    on('session.start', (_, e) => ({ cwd: e.cwd }))

    await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })

    expect(headers[0]?.['x-api-key']).toBe('dev-secret')
  },
)

test('a 401 names the option that fixes it', async ($, on) => {
  const lines: (string | undefined)[] = []

  on('http.fetch', () => ({
    value: { status: 401, ok: false, headers: {}, text: '{}' },
  }))
  on('ui.status', (_, e) => {
    lines.push(e.text)
    return { value: undefined }
  })
  on('command.register', () => ({ value: { command: 'agent-usage' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))

  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })

  expect(lines.at(-1)).toContain('gateway 401')
  expect(lines.at(-1)).toContain('auth_token')
})

test(
  'the session token stays home: a remote base_url gets none',
  { options: { base_url: 'https://ocg.example.com' } },
  async ($, on) => {
    const headers: (Record<string, string> | undefined)[] = []

    mock.env(on, { ANTHROPIC_AUTH_TOKEN: 'session-token' })
    on('http.fetch', (_, e) => {
      headers.push(e.init?.headers)
      return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify(BASE) } }
    })
    on('ui.status', () => ({ value: undefined }))
    on('command.register', () => ({ value: { command: 'agent-usage' } }))
    on('session.start', (_, e) => ({ cwd: e.cwd }))

    await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })

    // The fallback exists for the local gateway; handing the session token to
    // whatever host an option names would leak a credential.
    expect(headers[0]?.['x-api-key']).toBeUndefined()
  },
)

test(
  'an explicit auth_token still goes wherever base_url points',
  { options: { base_url: 'https://ocg.example.com', auth_token: 'remote-secret' } },
  async ($, on) => {
    const headers: (Record<string, string> | undefined)[] = []

    on('http.fetch', (_, e) => {
      headers.push(e.init?.headers)
      return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify(BASE) } }
    })
    on('ui.status', () => ({ value: undefined }))
    on('command.register', () => ({ value: { command: 'agent-usage' } }))
    on('session.start', (_, e) => ({ cwd: e.cwd }))

    await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })

    expect(headers[0]?.['x-api-key']).toBe('remote-secret')
  },
)
