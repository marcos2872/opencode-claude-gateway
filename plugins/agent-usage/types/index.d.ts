/**
 * What the band and `/agent-usage` read: the last report `GET /usage` on the
 * local gateway answered.
 */
export type UsagePayment = {
  /** Rolling 5h/weekly/monthly windows, as the gateway reports them. */
  windows: {
    rolling?: UsageWindow
    weekly?: UsageWindow
    monthly?: UsageWindow
  } | null
  /** Prepaid console credits in micro-cents (1 credit = US$ 0.01). */
  credits: {
    available_micro_cents: number
    limit_micro_cents?: number | null
  } | null
  /** Why the last probe failed, or null. */
  error: string | null
  /** Unix seconds when the gateway built this report. */
  fetchedAt: number
}

export type UsageWindow = {
  percent?: number | null
  status?: string | null
  resets_at?: string | null
}

declare module 'claude-code' {
  interface PluginState {
    'agent-usage': {
      last: Shaped<UsagePayment | null>
    }
  }
}
