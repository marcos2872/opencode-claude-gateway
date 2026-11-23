import { afterAll, beforeAll, describe, expect, test } from "bun:test"

import { callRemote, type RemoteRelay } from "./browser-mcp"

/**
 * `callRemote` fala com o relay de outro processo (o do OpenCode) por
 * `POST /tool`. O relay real responde os erros de ferramenta com status 5xx e
 * um corpo `{"ok":false,"error":"..."}` — o motivo de verdade fica no corpo.
 * Estes testes fixam duas coisas:
 *
 * 1. o corpo de erro **não** é descartado (o bug: `HTTP 500` sem mensagem);
 * 2. o corpo só é lido uma vez e um corpo inválido não derruba a chamada.
 */

type Reply = { status: number; body: string }

/** Relay falso: devolve o que o teste pedir para cada tool. */
let server: Bun.Server<undefined>
let relay: RemoteRelay
let replies: Record<string, Reply> = {}
let lastRequest: { tool: string; args: unknown; token: string | null } | null = null

beforeAll(() => {
  server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      const url = new URL(request.url)
      if (url.pathname !== "/tool") return new Response("Not found", { status: 404 })
      return request.json().then((body) => {
        const { tool, args } = body as { tool: string; args: unknown }
        lastRequest = { tool, args, token: request.headers.get("x-relay-token") }
        const reply = replies[tool] ?? {
          status: 200,
          body: JSON.stringify({ ok: true, content: `ran ${tool}` }),
        }
        return new Response(reply.body, {
          status: reply.status,
          headers: { "content-type": "application/json" },
        })
      })
    },
  })
  relay = { port: server.port as number, token: "test-token" }
})

afterAll(() => {
  server.stop(true)
})

describe("callRemote", () => {
  test("sucesso devolve o conteúdo do relay", async () => {
    replies = {}
    const result = await callRemote(relay, "read_page", { tabId: 1 })
    expect(result).toEqual({ ok: true, content: "ran read_page", images: undefined, metadata: undefined })
    expect(lastRequest).toEqual({ tool: "read_page", args: { tabId: 1 }, token: "test-token" })
  })

  test("500 de ref velha mantém a mensagem do relay", async () => {
    // O que o relay do OpenCode responde a uma ref de página já navegada.
    replies = { computer: { status: 500, body: JSON.stringify({ ok: false, error: "Ref ref_1 not found." }) } }
    const result = await callRemote(relay, "computer", { action: "click", ref: "ref_1" })
    expect(result).toEqual({ ok: false, error: "relay respondeu HTTP 500: Ref ref_1 not found." })
  })

  test("500 de navegação não-http mantém a mensagem do relay", async () => {
    replies = {
      navigate: { status: 500, body: JSON.stringify({ ok: false, error: "Only http(s) navigation is allowed." }) },
    }
    const result = await callRemote(relay, "navigate", { url: "about:blank" })
    expect(result).toEqual({
      ok: false,
      error: "relay respondeu HTTP 500: Only http(s) navigation is allowed.",
    })
  })

  test("503 sem extensão mantém a mensagem do relay", async () => {
    replies = { read_page: { status: 503, body: JSON.stringify({ ok: false, error: "Extensão não conectada." }) } }
    const result = await callRemote(relay, "read_page", {})
    expect(result).toEqual({ ok: false, error: "relay respondeu HTTP 503: Extensão não conectada." })
  })

  test("403 mantém a instrução acionável do token", async () => {
    replies = { read_page: { status: 403, body: "Forbidden" } }
    const result = await callRemote(relay, "read_page", {})
    expect(result.ok).toBe(false)
    expect(result.ok === false && result.error).toStartWith("relay recusou o token (403)")
  })

  test("corpo de erro não-JSON cai no status puro, sem quebrar", async () => {
    replies = { read_page: { status: 502, body: "<html>bad gateway</html>" } }
    const result = await callRemote(relay, "read_page", {})
    expect(result).toEqual({ ok: false, error: "relay respondeu HTTP 502" })
  })

  test("200 com ok:false devolve o erro do corpo", async () => {
    replies = { read_page: { status: 200, body: JSON.stringify({ ok: false, error: "falha no relay" }) } }
    const result = await callRemote(relay, "read_page", {})
    expect(result).toEqual({ ok: false, error: "falha no relay" })
  })
})
