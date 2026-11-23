import { expect, mock, test } from 'claude-code/testing'

/**
 * The gateway's answer is played by an `http.fetch` hook beneath the plugin,
 * the band is drawn through `$.ui.mount` on `AbovePrompt`, and the status line
 * is read back from the `ui.status` hook the plugin raises.
 */
/** One OpenCode Go window, as `GET /usage` reports it. */
type Window = { percent: number; status: string; resets_at: string | null }

type Body = {
  provider: string
  windows: { rolling: Window; weekly: Window; monthly: Window }
  credits: { available_micro_cents: number; limit_micro_cents: number | null }
  error: string | null
  status: number | null
  fetched_at: number
}

const BASE: Body = {
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

/** The props `AbovePrompt` hands a row's hook; only the width matters here. */
const BAND = (columns: number) => ({
  hasSurvey: false,
  isCollapsed: false,
  isWorking: false,
  maxRows: 12,
  bodyColumns: columns,
  scroll: { offset: 0, bodyRows: 12 },
  view: {},
})

const session = ($: any) => $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })

/** The band's row elements, in order, from the mounted drawing. */
async function bandRows(ui: any): Promise<any[]> {
  const boxes = await ui.findAll({ type: 'Box' })
  return boxes.filter((row: any) => String(row.key ?? '').startsWith('row-'))
}

/** Every string a row shows, with no space where the layout put a gap. */
function rowText(row: any): string {
  const walk = (node: any): string => {
    if (typeof node === 'string') return node
    if (!node || typeof node !== 'object') return ''
    if (typeof node.text === 'string' && node.text !== '') return node.text
    const children = Array.isArray(node.children) ? node.children : []
    return children.map(walk).join('')
  }
  return walk(row)
}

/**
 * How many cells a row occupies: its text, plus the width each blank gap
 * declares. A gap is a width-only `Box`, never a `Text` of spaces — spaces
 * collapse when painted, which is what broke the alignment before.
 */
function rowWidth(row: any): number {
  const walk = (node: any): number => {
    if (typeof node === 'string') return node.length
    if (!node || typeof node !== 'object') return 0
    if (typeof node.text === 'string' && node.text !== '') return node.text.length
    const own = typeof node.props?.width === 'number' ? node.props.width : 0
    const children = Array.isArray(node.children) ? node.children : []
    return own + children.reduce((n: number, child: any) => n + walk(child), 0)
  }
  if (Array.isArray(row.children)) {
    return row.children.reduce((n: number, child: any) => n + walk(child), 0)
  }
  return walk(row)
}

/** Mounts the band at `columns` and reads the rows it drew. */
async function drawBand($: any, columns: number): Promise<{ text: string; width: number }[]> {
  const ui = await $.ui.mount({
    plugin: 'agent-usage',
    surface: 'terminal',
    component: 'AbovePrompt',
    props: BAND(columns),
  })
  const rows = (await bandRows(ui)).map((row: any) => ({ text: rowText(row), width: rowWidth(row) }))
  await ui.unmount()
  return rows
}

test('the band draws a bar, a percentage, a reset and the credits', async ($, on) => {
  on('http.fetch', () => ({
    value: { status: 200, ok: true, headers: {}, text: JSON.stringify(BASE) },
  }))
  on('command.register', () => ({ value: { command: 'agent-usage' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))

  await session($)
  const rows = await drawBand($, 100)

  // Wide enough for the header, the three bars and the credits on one row.
  expect(rows[0]?.text).toContain('OpenCode Go')
  expect(rows[0]?.text).toContain('5h')
  expect(rows[0]?.text).toContain('6%')
  expect(rows[0]?.text).toContain('wk')
  expect(rows[0]?.text).toContain('28%')
  expect(rows[0]?.text).toContain('mo')
  expect(rows[0]?.text).toContain('41%')
  expect(rows[0]?.text).toContain('US$ 3.65')
  // A bar of sixteen cells: filled for the share, blank for the rest.
  expect(rows[0]?.text).toContain('█')
  expect(rows[0]?.text).toContain('░')
  // The reset row lines each reset up under its own bar.
  expect(rows[1]?.text).toContain('↻ 2h14m')
  expect(rows[1]?.text).toContain('↻ 6d3h')
  expect(rows[1]?.text).toContain('↻ 15d19h')
  // Gaps are real width, not a run of spaces that would collapse: the row
  // occupies every column the bars, labels and credits need.
  expect(rows[0]?.width).toBeGreaterThan(rows[0]?.text.length ?? 0)
})

test('the band reflows to the columns it is given, never past them', async ($, on) => {
  on('http.fetch', () => ({
    value: { status: 200, ok: true, headers: {}, text: JSON.stringify(BASE) },
  }))
  on('command.register', () => ({ value: { command: 'agent-usage' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))

  await session($)

  for (const columns of [130, 115, 100, 72, 56, 40, 25, 12]) {
    const rows = await drawBand($, columns)
    for (const row of rows) {
      expect(row.width).toBeLessThanOrEqual(columns)
    }
    // Every width keeps the windows readable, however much it had to drop.
    const joined = rows.map(r => r.text).join('\n')
    expect(joined).toContain('5h')
  }

  // A narrow band drops the bars and keeps the numbers.
  const narrow = (await drawBand($, 40)).map(r => r.text).join('\n')
  expect(narrow).not.toContain('█')
  expect(narrow).toContain('5h 6%')
  expect(narrow).toContain('US$ 3.65')

  // A band with room for bars but not for the credit beside them stacks it.
  const stacked = await drawBand($, 56)
  expect(stacked[0]?.text).toContain('█')
  expect(stacked[1]?.text).toContain('US$ 3.65')
  expect(stacked[1]?.text).toContain('↻')
})

test('a window past the warn threshold is red and carries a mark', async ($, on) => {
  let body = BASE
  on('http.fetch', () => ({
    value: { status: 200, ok: true, headers: {}, text: JSON.stringify(body) },
  }))
  on('command.register', () => ({ value: { command: 'agent-usage' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  const clock = mock.clock(on)

  await session($)

  // 56% is the middle step: amber, no mark.
  body = {
    ...BASE,
    windows: { ...BASE.windows, monthly: { ...BASE.windows.monthly, percent: 56 } },
  }
  await clock.advance(60_000)
  const middle = await drawBand($, 100)
  expect(middle.map(r => r.text).join('\n')).toContain('56%')
  expect(middle.map(r => r.text).join('\n')).not.toContain('!')

  body = {
    ...BASE,
    windows: { ...BASE.windows, monthly: { ...BASE.windows.monthly, percent: 92 } },
  }
  await clock.advance(60_000)
  const hot = await drawBand($, 100)
  expect(hot.map(r => r.text).join('\n')).toContain('92%!')

  body = {
    ...BASE,
    windows: {
      ...BASE.windows,
      rolling: { percent: 100, status: 'rate-limited', resets_at: null },
    },
  }
  await clock.advance(60_000)
  const limited = await drawBand($, 100)
  expect(limited.map(r => r.text).join('\n')).toContain('100% limit')
})

test('a quota failure keeps the last value and marks it stale', async ($, on) => {
  let body = BASE
  on('http.fetch', () => ({
    value: { status: 200, ok: true, headers: {}, text: JSON.stringify(body) },
  }))
  on('command.register', () => ({ value: { command: 'agent-usage' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  const clock = mock.clock(on)

  await session($)
  expect((await drawBand($, 100)).map(r => r.text).join('\n')).not.toContain('stale')

  body = { ...BASE, error: 'HTTP 401 unauthorized — run `opencode auth login` again' }
  await clock.advance(60_000)

  const stale = await drawBand($, 100)
  expect(stale.map(r => r.text).join('\n')).toContain('stale')
  // The numbers stay: a failed refresh does not blank the quota.
  expect(stale.map(r => r.text).join('\n')).toContain('US$ 3.65')
})

test('a good read leaves the status line empty, since the band carries the numbers', async ($, on) => {
  const lines: (string | undefined)[] = []
  on('http.fetch', () => ({
    value: { status: 200, ok: true, headers: {}, text: JSON.stringify(BASE) },
  }))
  on('ui.status', (_, e) => {
    lines.push(e.text)
    return { value: undefined }
  })
  on('command.register', () => ({ value: { command: 'agent-usage' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))

  await session($)

  // The band draws the same numbers; a second line repeating them is clutter.
  expect(lines.at(-1)).toBeUndefined()
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

  await session($)

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

    await session($)

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

  await session($)

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

    await session($)

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

    await session($)

    expect(headers[0]?.['x-api-key']).toBe('remote-secret')
  },
)
