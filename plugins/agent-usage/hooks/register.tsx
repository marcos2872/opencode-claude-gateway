import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { UsagePayment, UsageWindow } from '../types'

/** Where the local gateway answers (the config menu's `base_url`). */
const DEFAULT_BASE = 'http://127.0.0.1:3737'
/** OpenCode Go windows move in hours; a minute of staleness is invisible. */
const POLL_MS = 60_000
/** Warn on the status line from here up (the Noctalia plugin's threshold). */
const WARN_PERCENT = 90

const last = atom(
  { plugin: 'agent-usage', key: 'last' } as const,
  null as UsagePayment | null,
  { shape: 'usage-1' },
)

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
 * One read of `GET /usage`, then the status line. A failure keeps the last
 * known value and says why, so a gateway restart never blanks the quota.
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
    $.ui.status(render(payment))
  } catch (e) {
    $.ui.status(`gateway unreachable (${(e as Error).message})`)
  }
}

export const register: Register = (on, options) => {
  base =
    typeof options.base_url === 'string' && options.base_url !== ''
      ? options.base_url
      : DEFAULT_BASE
  token = typeof options.auth_token === 'string' ? options.auth_token : ''

  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'agent-usage',
      description: 'OpenCode Go quota: 5h/weekly/monthly windows + console credits',
    })
    // First value as soon as the session opens; there is no pane to draw,
    // only the pinned line under the prompt.
    await poll($)
    $.clock.every(POLL_MS, () => poll($))

    return next(e)
  })

  on('command.run', { command: 'agent-usage' }, async $ => {
    await poll($)
    const payment = await read($, last)

    return { text: describe(payment) }
  })
}

/**
 * One line with all three windows, each with its own reset, then the credits:
 *
 *   5h 5% (2h14m) · wk 4% (6d3h) · mo 41% (15d19h) · US$ 3.65
 *
 * The plugin name is not written here: the engine prefixes the status line
 * with it. A window past the warn threshold carries `!`, and a rate-limited
 * one `limit`.
 */
function render(payment: UsagePayment): string {
  if (!payment.windows && !payment.credits) {
    return payment.error ?? 'no data'
  }
  const parts: string[] = []
  for (const [label, key] of [
    ['5h', 'rolling'],
    ['wk', 'weekly'],
    ['mo', 'monthly'],
  ] as const) {
    const window = payment.windows?.[key]
    if (!window) continue
    const pct = Math.round(window.percent ?? 0)
    const reset = resetIn(window)
    let mark = ''
    if (window.status === 'rate-limited' || pct >= 100) mark = ' limit'
    else if (pct >= WARN_PERCENT) mark = '!'
    parts.push(`${label} ${pct}%${mark}${reset ? ` (${reset})` : ''}`)
  }
  if (payment.credits) {
    parts.push(money(payment.credits.available_micro_cents))
  }
  const stale = payment.error ? ' (stale)' : ''
  return `${parts.join(' · ')}${stale}`
}

/** The full report `/usage` prints: one line per window, then the credits. */
function describe(payment: UsagePayment | null): string {
  if (!payment) return 'No usage report yet.'
  const lines: string[] = []
  for (const [label, key] of [
    ['5 hours', 'rolling'],
    ['Weekly', 'weekly'],
    ['Monthly', 'monthly'],
  ] as const) {
    const window = payment.windows?.[key]
    lines.push(
      window
        ? `${label}: ${Math.round(window.percent ?? 0)}% used` +
            (window.status ? ` (${window.status})` : '') +
            (resetIn(window) ? `, ${resetIn(window)}` : '')
        : `${label}: —`,
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

/** "resets in 2h14m", from the ISO or epoch-seconds forms the gateway keeps. */
function resetIn(window: UsageWindow): string {
  if (!window.resets_at) return ''
  const at = Date.parse(window.resets_at)
  const ms = Number.isNaN(at) ? Number(window.resets_at) * 1000 : at
  if (!Number.isFinite(ms) || ms <= 0) return ''
  const seconds = Math.round((ms - Date.now()) / 1000)
  if (seconds <= 0) return 'reset due'
  return `resets in ${duration(seconds)}`
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
