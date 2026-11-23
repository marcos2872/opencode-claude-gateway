import { atom, read, update } from 'claude-code'
import type {
  BoxProps,
  ElementConstructor,
  EngineInterface,
  Register,
  RenderElement,
  TextProps,
} from 'claude-code'

import type { UsagePayment, UsageWindow } from '../types'

/** Where the local gateway answers (the config menu's `base_url`). */
const DEFAULT_BASE = 'http://127.0.0.1:3737'
/** OpenCode Go windows move in hours; a minute of staleness is invisible. */
const POLL_MS = 60_000
/** From here up a window is drawn as a problem (red, with `!`). */
const WARN_PERCENT = 90
/** From here up the percentage itself leaves the page's color. */
const MID_PERCENT = 50

/**
 * How wide the bars are, in cells. The layout tries the widest first and keeps
 * the first one whose rows all fit `bodyColumns`, so a narrow terminal (or a
 * long `limit` mark) drops a step instead of wrapping.
 */
const BAR_STEPS = [14, 10, 8] as const

const last = atom(
  { plugin: 'agent-usage', key: 'last' } as const,
  null as UsagePayment | null,
  { shape: 'usage-1' },
)

/**
 * Whether this activation already started the poll loop. `register` runs again
 * on a hot reload, which resets this and lets `engine.create` read once more.
 */
let loaded = false

/** Filled in `register` from the manifest's `userConfig`. */
let base = DEFAULT_BASE
/** Config `auth_token`, or the session's own token when that is empty. */
let token = ''

const endpoint = () => `${base.replace(/\/+$/, '')}/usage`

/**
 * Whether `base` names this machine: `127.0.0.1`, `[::1]` or `localhost`.
 * The gateway listens on loopback only, so anything else is not the gateway
 * and must never be handed the session's credential by the fallback below.
 */
function isLoopback(url: string): boolean {
  try {
    const host = new URL(url).hostname
    return host === '127.0.0.1' || host === 'localhost' || host === '[::1]'
  } catch {
    return false
  }
}

/**
 * The gateway token: the plugin's `auth_token`, else the `ANTHROPIC_AUTH_TOKEN`
 * the session already uses to reach the gateway. Without that fallback a
 * gateway started with an `auth_token` (`--settings` with a dev token, say)
 * answers 401 here while the conversation keeps working, since the plugin's
 * request carries its own headers and inherits nothing from the CLI.
 *
 * The fallback is loopback-only: a `base_url` pointing anywhere else is not
 * this gateway, and sending it the session token would hand a credential to
 * whatever host an option names. An explicit `auth_token` still goes wherever
 * `base_url` says, since the person chose both.
 *
 * `$.env` may be absent (the test kit without `mock.env`), so a throw is not
 * an error: the request just goes out unauthenticated as before.
 */
async function sessionToken($: EngineInterface): Promise<string> {
  if (token !== '') return token
  if (!isLoopback(base)) return ''
  try {
    return (await $.env.get('ANTHROPIC_AUTH_TOKEN')) ?? ''
  } catch {
    return ''
  }
}

/**
 * One read of `GET /usage`, stored for the band. The status line keeps a
 * one-line summary: the band is collapsed (or a surface does not draw it)
 * exactly when that line is all the person has. A failed fetch keeps the last
 * value, marked `stale` by both.
 *
 * Top-level on purpose: `$` may only be handed to a function declared here.
 */
async function poll($: EngineInterface) {
  try {
    const headers: Record<string, string> = { accept: 'application/json' }
    const credential = await sessionToken($)
    if (credential !== '') headers['x-api-key'] = credential
    const res = await $.http.fetch(endpoint(), { headers })
    if (!res.ok) {
      // 404 means the gateway predates `GET /usage`: say so, since updating
      // the binary is the fix and nothing else in the line would hint at it.
      // 401 is the token mismatch (the gateway's `auth_token` is not the one
      // this session uses), so name the option that fixes it.
      $.ui.status(
        res.status === 404
          ? 'gateway has no /usage (update ocg)'
          : res.status === 401
            ? 'gateway 401 (set plugin auth_token)'
            : `gateway HTTP ${res.status}`,
      )
      return
    }
    const body = JSON.parse(res.text) as {
      windows?: UsagePayment['windows']
      credits?: UsagePayment['credits']
      error?: string | null
      fetched_at?: number
    }
    const payment: UsagePayment = {
      windows: body.windows ?? null,
      credits: body.credits ?? null,
      error: body.error ?? null,
      fetchedAt: body.fetched_at ?? Math.floor(Date.now() / 1000),
    }
    await update($, last, () => payment)
    // The band draws the numbers; a second line with the same numbers is
    // clutter, so the status line is cleared on every good read and only ever
    // carries what the band cannot (a gateway that will not answer).
    $.ui.status(undefined)
  } catch (e) {
    $.ui.status(`gateway unreachable (${(e as Error).message})`)
  }
}

/**
 * One read right away, then one a minute: the only thing that talks to the
 * gateway. Idempotent per activation, so both anchors can call it.
 */
async function startPolling($: EngineInterface): Promise<void> {
  if (loaded) return
  loaded = true
  await poll($)
  try {
    $.clock.every(POLL_MS, () => poll($))
  } catch {
    // A surface without a clock (a `-p` run) gets the one read.
  }
}

export const register: Register = (on, options) => {
  base =
    typeof options.base_url === 'string' && options.base_url !== ''
      ? options.base_url
      : DEFAULT_BASE
  token = typeof options.auth_token === 'string' ? options.auth_token : ''

  /**
   * The poll loop, started by whichever of these comes first:
   *
   * - `plugin.register`, which fires once per load **and per reload** with `$`
   *   whole. `claude --reload-plugins` re-runs `register` but fires no
   *   `session.start`, so a loop living only there left the band on
   *   `sem dados ainda` forever after a reload, with the gateway answering
   *   fine. It judges nothing: answering `next(e)` without a `refuse` lets the
   *   module join as it always did.
   * - `session.start`, the ordinary path, and the one a test kit raises.
   *
   * `loaded` keeps one read and one timer per activation, whichever fired.
   */
  on('plugin.register', async ($, e, next) => {
    await startPolling($)
    return next(e)
  })

  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'agent-usage',
      description: 'OpenCode Go quota: 5h/weekly/monthly windows + console credits',
    })
    await startPolling($)

    return next(e)
  })

  // One row of the band above the prompt, once for this plugin. A survey holds
  // the band while it is up, so this yields there; collapsing the band is the
  // person's business (ctrl+x ctrl+a) and the status line covers it.
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (e.props.hasSurvey) return next(e)
    // Reading the atom while drawing subscribes this drawing to later writes,
    // so the minute's poll redraws the band by itself.
    const payment = await read($, last)
    const { Box, Text } = $.ui.resolve(e)
    return band(Box, Text, layout(payment, e.props.bodyColumns))
  })

  on('command.run', { command: 'agent-usage' }, async $ => {
    await poll($)
    const payment = await read($, last)

    return { text: describe(payment) }
  })
}

// ─── the band ───────────────────────────────────────────────────────────────

/** A theme color for a run of text; `subtle` is the quiet one. */
type Tone = 'text' | 'subtle' | 'success' | 'warning' | 'error'

/**
 * One piece of a row: text drawn in a tone, or blank space of a fixed width.
 *
 * Space is a `Box` with an explicit `width` and never a `Text` of spaces: a
 * run of spaces does not keep its cells when the terminal paints it, which is
 * exactly what slid the windows into each other and left each `↻` outside its
 * bar. A width is a width on every surface.
 */
type Cell = { text: string; tone: Tone; dim?: boolean } | { pad: number }

/** One row of the band, the unit the layouts produce. */
type Row = Cell[]

/** The band's tree: one `Text` per text cell, a width-only `Box` per gap. */
function band(
  Box: ElementConstructor<BoxProps>,
  Text: ElementConstructor<TextProps>,
  rows: Row[],
): RenderElement {
  return (
    <Box key="agent-usage" flexDirection="column">
      {rows.map((row, i) => (
        <Box key={`row-${i}`} flexDirection="row">
          {row.map((cell, j) => {
            if ('pad' in cell) return <Box key={`c-${j}`} width={cell.pad} />
            return (
              <Text key={`c-${j}`} color={cell.tone} dimColor={cell.dim}>
                {cell.text}
              </Text>
            )
          })}
        </Box>
      ))}
    </Box>
  )
}

/**
 * The report as rows of cells, for the columns the band has left. Pure — no
 * engine and no clock — so the layout is testable on its own.
 *
 *    OpenCode Go 5h ░░░░░░░░░░░░░░   1%   wk ░░░░░░░░░░░░░░   3%   mo ████████░░░░░░  56%    US$ 0.66
 *                   ↻ 3h53m                  ↻ 6d1h                  ↻ 8d20h
 *
 * A ladder of attempts, widest first, and the first whose rows all fit wins:
 * bars with a reset under each; then the same without the header; then
 * narrower bars; then no resets (the tightest one rides beside the credits);
 * then the credits on a row of their own; below that, plain numbers.
 */
function layout(payment: UsagePayment | null, columns: number): Row[] {
  const cols = Math.max(1, Math.floor(columns))
  const windows: [string, UsageWindow][] = []
  for (const [name, window] of [
    ['5h', payment?.windows?.rolling],
    ['wk', payment?.windows?.weekly],
    ['mo', payment?.windows?.monthly],
  ] as [string, UsageWindow | undefined][]) {
    if (window) windows.push([name, window])
  }
  const credits = payment?.credits ? money(payment.credits.available_micro_cents) : ''
  const stale = payment?.error ? ' stale' : ''

  if (windows.length === 0 && credits === '') {
    return ensure(
      [
        [
          { text: 'OpenCode Go', tone: 'text' },
          { pad: 1 },
          { text: '·', tone: 'subtle', dim: true },
          { pad: 1 },
          { text: payment?.error ?? 'sem dados ainda', tone: 'warning' },
        ],
      ],
      cols,
    )
  }

  // The header is dropped before the bars narrow: the bars carry the numbers,
  // the header only carries the name.
  for (const bar of BAR_STEPS) {
    for (const titleToo of [true, false]) {
      const rows = withResets(windows, credits, stale, bar, titleToo, cols)
      if (fits(rows, cols)) return ensure(rows, cols)
    }
  }
  for (const bar of BAR_STEPS) {
    for (const titleToo of [true, false]) {
      const rows = withoutResets(windows, credits, stale, bar, titleToo, cols)
      if (fits(rows, cols)) return ensure(rows, cols)
    }
    for (const titleToo of [true, false]) {
      const rows = stacked(windows, credits, stale, bar, titleToo)
      if (fits(rows, cols)) return ensure(rows, cols)
    }
  }
  // Last resort: numbers, first whole, then with the credit dropped, then cut.
  for (const row of [numbers(windows, credits, stale), numbers(windows, '', stale)]) {
    if (fits([row], cols)) return ensure([row], cols)
  }
  return ensure([cut(numbers(windows, credits, stale), cols)], cols)
}

/** Every row of a laid-out report really fits: the last guard before drawing. */
function ensure(rows: Row[], cols: number): Row[] {
  return rows.map(row => (width(row) <= cols ? row : cut(row, cols)))
}

/** Cuts a row to `cols` cells, ending it in `…` (only the numbers row needs it). */
function cut(row: Row, cols: number): Row {
  const out: Row = []
  let used = 0
  for (const cell of row) {
    const room = cols - used
    if (room <= 0) break
    if ('pad' in cell) {
      const take = Math.min(cell.pad, room)
      out.push({ pad: take })
      used += take
      continue
    }
    if (cell.text.length <= room) {
      out.push(cell)
      used += cell.text.length
      continue
    }
    // One cell goes to the ellipsis, so the row never exceeds `cols`.
    out.push({ ...cell, text: cell.text.slice(0, Math.max(0, room - 1)) + '…' })
    used = cols
    break
  }
  return out
}

/**
 * Bars with a reset under each, and the header leading the row. Under the bar
 * rather than under the label, because that is the number the reset belongs to.
 */
function withResets(
  windows: [string, UsageWindow][],
  credits: string,
  stale: string,
  bar: number,
  withTitle: boolean,
  cols: number,
): Row[] {
  const head: Row = withTitle ? [...title(), { pad: 1 }] : []
  // The reset row starts under the first bar, so it leads with the header's width.
  const under: Row = withTitle ? [{ pad: titleWidth() + 1 }] : []
  windows.forEach(([name, window], i) => {
    if (i > 0) {
      head.push({ pad: 3 })
      under.push({ pad: 3 })
    }
    const column = group(name, window, bar)
    head.push(...column.head)
    under.push(...column.under)
  })
  return [end(head, credits, stale, cols), trim(under)]
}

/** Bars without the reset row: the tightest reset rides along. */
function withoutResets(
  windows: [string, UsageWindow][],
  credits: string,
  stale: string,
  bar: number,
  withTitle: boolean,
  cols: number,
): Row[] {
  const head: Row = withTitle ? [...title(), { pad: 1 }] : []
  windows.forEach(([name, window], i) => {
    if (i > 0) head.push({ pad: 2 })
    head.push(...group(name, window, bar).head)
  })
  const row = end(head, credits, stale, cols)
  const tight = tightest(windows)
  if (!tight) return [row]
  return [
    row,
    [
      { text: `↻ ${left(tight[1])}`, tone: 'subtle' },
      { pad: 2 },
      { text: `(${tight[0]} aperta primeiro)`, tone: 'subtle', dim: true },
    ],
  ]
}

/** Bars on one row, credits and the tightest reset on the next. */
function stacked(
  windows: [string, UsageWindow][],
  credits: string,
  stale: string,
  bar: number,
  withTitle: boolean,
): Row[] {
  const head: Row = withTitle ? [...title(), { pad: 1 }] : []
  windows.forEach(([name, window], i) => {
    if (i > 0) head.push({ pad: 2 })
    head.push(...group(name, window, bar).head)
  })
  const rest: Row = []
  if (credits) rest.push({ text: credits, tone: 'text' })
  const tight = tightest(windows)
  if (tight) {
    if (rest.length > 0) {
      rest.push({ pad: 1 }, { text: '·', tone: 'subtle', dim: true }, { pad: 1 })
    }
    rest.push({ text: `↻ ${left(tight[1])}`, tone: 'subtle' }, { pad: 1 })
    rest.push({ text: `(${tight[0]})`, tone: 'subtle', dim: true })
  }
  if (stale) rest.push({ pad: 1 }, { text: 'stale', tone: 'warning' })
  return [trim(head), trim(rest)]
}

/** Numbers only, on one row: the fallback for a terminal too narrow for bars. */
function numbers(windows: [string, UsageWindow][], credits: string, stale: string): Row {
  const row: Row = []
  windows.forEach(([name, window], i) => {
    if (i > 0) row.push({ pad: 1 }, { text: '·', tone: 'subtle', dim: true }, { pad: 1 })
    row.push({
      text: `${name} ${Math.round(window.percent ?? 0)}%${mark(window)}`,
      tone: toneOf(window),
    })
  })
  if (credits) row.push({ pad: 1 }, { text: '·', tone: 'subtle', dim: true }, { pad: 1 }, { text: credits, tone: 'text' })
  if (stale) row.push({ pad: 1 }, { text: 'stale', tone: 'warning' })
  return row
}

/** Every row of an attempt fits in `cols`. */
function fits(rows: Row[], cols: number): boolean {
  return rows.every(row => width(row) <= cols)
}

/** `OpenCode Go` — the band names itself; only the status line gets a prefix. */
function title(): Row {
  return [{ text: 'OpenCode Go', tone: 'text' }]
}

const titleWidth = (): number => 'OpenCode Go'.length

/**
 * One window as the two rows of its column: `5h ████░░░░  56%` and, under the
 * bar, its reset. Both rows are padded to one width so the next column starts
 * at the same place in either.
 */
function group(name: string, window: UsageWindow, bar: number): { head: Row; under: Row } {
  const pct = Math.round(window.percent ?? 0)
  const filled = pct <= 0 ? 0 : Math.min(bar, Math.max(1, Math.round((pct / 100) * bar)))
  const tone = toneOf(window)
  const pctText = `${pct}%`
  const head: Row = [
    { text: name, tone: 'subtle' },
    { pad: 3 - name.length },
    { text: '█'.repeat(filled), tone },
    { text: '░'.repeat(bar - filled), tone: 'subtle', dim: true },
    { pad: 4 - pctText.length },
    { text: pctText, tone: pct >= MID_PERCENT ? tone : 'text' },
  ]
  const markText = mark(window)
  if (markText) head.push({ pad: 1 }, { text: markText, tone: 'error' })
  const remaining = secondsLeft(window)
  // Three cells in: directly under the first cell of the bar.
  const under: Row =
    remaining === null
      ? []
      : [{ pad: 3 }, { text: `↻ ${left(window)}`, tone: 'subtle' }]
  const w = Math.max(width(head), width(under))
  return { head: pad(head, w), under: pad(under, w) }
}

/**
 * Row 1 with the credits and the ` stale` mark at the right edge, the gap
 * computed from `cols` so a changing percentage never drags the credits
 * around. Without a credit the row is left as it is.
 */
function end(row: Row, credits: string, stale: string, cols: number): Row {
  const tail: Row = []
  if (credits) tail.push({ text: credits, tone: 'text' })
  if (stale) tail.push({ pad: 1 }, { text: 'stale', tone: 'warning' })
  if (tail.length === 0) return trim(row)
  const room = cols - width(row) - width(tail)
  return [...row, { pad: Math.max(1, room) }, ...tail]
}

/** Cells wide a row is: text and pads counted, the flexible gap as nothing. */
function width(row: Row): number {
  let n = 0
  for (const cell of row) {
    n += 'pad' in cell ? cell.pad : cell.text.length
  }
  return n
}

/** Pads a row to `w` cells (a no-op when it already is wider). */
function pad(row: Row, w: number): Row {
  const missing = w - width(row)
  return missing > 0 ? [...row, { pad: missing }] : row
}

/** Drops trailing blanks, so a row never carries invisible width. */
function trim(row: Row): Row {
  let end = row.length
  while (end > 0) {
    const cell = row[end - 1]
    if (!cell || !('pad' in cell) || cell.pad <= 0) break
    end--
  }
  return end === row.length ? row : row.slice(0, end)
}

/**
 * 0–49% is fine, 50–89% is worth watching, 90% up (or a rate-limited window)
 * is a problem. Only the middle step and up recolor the percentage itself, so
 * a quiet month stays quiet.
 */
function toneOf(window: UsageWindow): Tone {
  const pct = window.percent ?? 0
  if (window.status === 'rate-limited' || pct >= WARN_PERCENT) return 'error'
  if (pct >= MID_PERCENT) return 'warning'
  return 'success'
}

/** ` limit` for a rate-limited window, `!` past the warn threshold, else nothing. */
function mark(window: UsageWindow): string {
  if (window.status === 'rate-limited') return ' limit'
  return (window.percent ?? 0) >= WARN_PERCENT ? '!' : ''
}

/** The window that resets soonest: the one binding the session right now. */
function tightest(windows: [string, UsageWindow][]): [string, UsageWindow] | null {
  let best: [string, UsageWindow] | null = null
  let soonest = Infinity
  for (const entry of windows) {
    const seconds = secondsLeft(entry[1])
    if (seconds === null || seconds >= soonest) continue
    best = entry
    soonest = seconds
  }
  return best
}

/** `3h53m`, or `agora` once the window is due. */
function left(window: UsageWindow): string {
  const seconds = secondsLeft(window)
  if (seconds === null) return '?'
  return seconds > 0 ? duration(seconds) : 'agora'
}

/** Seconds until the window resets; null when the gateway did not say. */
function secondsLeft(window: UsageWindow): number | null {
  if (!window.resets_at) return null
  const at = Date.parse(window.resets_at)
  const ms = Number.isNaN(at) ? Number(window.resets_at) * 1000 : at
  if (!Number.isFinite(ms) || ms <= 0) return null
  return Math.round((ms - Date.now()) / 1000)
}

/** The full report `/agent-usage` prints: a bar per window, then the credits. */
function describe(payment: UsagePayment | null): string {
  if (!payment) return 'No usage report yet.'
  const lines: string[] = []
  const bar = BAR_STEPS[0]
  for (const [label, key] of [
    ['5 hours', 'rolling'],
    ['Weekly', 'weekly'],
    ['Monthly', 'monthly'],
  ] as const) {
    const window = payment.windows?.[key]
    if (!window) {
      lines.push(`${label}: —`)
      continue
    }
    const pct = Math.round(window.percent ?? 0)
    const filled = pct <= 0 ? 0 : Math.min(bar, Math.max(1, Math.round((pct / 100) * bar)))
    const meter = '█'.repeat(filled) + '░'.repeat(bar - filled)
    const remaining = secondsLeft(window)
    lines.push(
      `${label.padEnd(8)} ${meter} ${String(pct).padStart(3)}%${mark(window)}` +
        (window.status && window.status !== 'ok' ? ` (${window.status})` : '') +
        (remaining === null
          ? ''
          : remaining > 0
            ? `, resets in ${duration(remaining)}`
            : ', reset due'),
    )
  }
  lines.push(
    payment.credits
      ? `Credits available: ${money(payment.credits.available_micro_cents)}`
      : 'Credits available: —',
  )
  if (payment.error) lines.push(`Last error: ${payment.error}`)
  const age = Math.floor(Date.now() / 1000) - payment.fetchedAt
  lines.push(`Updated ${age < 120 ? `${age}s` : `${Math.round(age / 60)}m`} ago`)
  return lines.join('\n')
}

/** 2d3h / 2h14m / 9m — coarser as the span grows, like the Noctalia panel. */
function duration(seconds: number): string {
  const days = Math.floor(seconds / 86_400)
  const hours = Math.floor((seconds % 86_400) / 3_600)
  const minutes = Math.floor((seconds % 3_600) / 60)
  if (days > 0) return `${days}d${hours}h`
  if (hours > 0) return `${hours}h${minutes}m`
  return `${Math.max(1, minutes)}m`
}

/** Micro-cents → "US$ 3.65": the console counts 1e-8 of a dollar. */
function money(microCents: number): string {
  return `US$ ${(microCents / 1e8).toFixed(2)}`
}
