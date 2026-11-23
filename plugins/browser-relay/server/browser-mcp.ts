#!/usr/bin/env bun
/**
 * Servidor MCP `browser` para o Claude Code: as abas do Chrome pela extensão
 * OpenCode Browser.
 *
 * O Claude Code não tem WebSocket nem servidor HTTP num mod de hooks, então o
 * relay vira um processo próprio falando MCP por stdio: a extensão continua
 * conectando em ws://127.0.0.1:8799 (ele descobre sozinha por `GET /health` nas
 * portas 8799-8802) e as tools do Chrome saem como `mcp__browser__*`.
 *
 * O protocolo com a extensão e o formato das tools são os mesmos do
 * `plugin/browser-relay.ts` do repositório opencode-browser-extension, então os
 * dois podem coexistir — mas não ao mesmo tempo na mesma porta: quem chega
 * primeiro fica com ela (o outro sobe na porta seguinte e a extensão prefere a
 * versão mais alta).
 *
 * Capturas de tela são gravadas em disco e também devolvidas como bloco de
 * imagem, que o MCP entrega ao modelo nativamente.
 */

import { lstatSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

// ---------------------------------------------------------------------------
// Protocolo com a extensão (igual ao browser-relay.ts do OpenCode)
// ---------------------------------------------------------------------------

const BASE_PORT = Number(process.env.OGB_MCP_PORT ?? process.env.OPENCODE_BROWSER_RELAY_PORT ?? 8799)
const PORT_CANDIDATES = [BASE_PORT, BASE_PORT + 1, BASE_PORT + 2, BASE_PORT + 3]
const EXPECTED_EXTENSION_ID =
  process.env.OPENCODE_BROWSER_EXTENSION_ID ?? "meapihkmghapjefepmkckpikedpimhin"
const ALLOW_ANY_EXTENSION = process.env.OPENCODE_BROWSER_ALLOW_ANY_EXTENSION === "1"
const SERVER_VERSION = 6
const CALL_TIMEOUT_MS = 180_000
const EXTENSION_GRACE_MS = 8_000
const MAX_WS_PAYLOAD = 24 * 1024 * 1024
const MAX_IMAGE_BASE64 = 8 * 1024 * 1024
const IMAGE_MIMES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"])
const IMAGE_EXT: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/webp": "webp",
  "image/gif": "gif",
}

type ImagePayload = { name: string; mime: string; data: string }

type ToolResponse = {
  ok: boolean
  content?: string
  images?: ImagePayload[]
  error?: string
  metadata?: Record<string, unknown>
}

type SocketData = {
  authed: boolean
  lastPong: number
  pingTimer?: ReturnType<typeof setInterval>
}

type Pending = {
  socket: { send(data: string): void }
  resolve: (value: ToolResponse) => void
  reject: (error: Error) => void
  timer: ReturnType<typeof setTimeout>
}

const state = {
  active: null as { send(data: string): void; data: SocketData } | null,
  pending: new Map<string, Pending>(),
  server: null as unknown,
  port: BASE_PORT,
}

function log(message: string, error?: unknown) {
  // stderr: stdout é o canal do MCP.
  const suffix = error instanceof Error ? `: ${error.message}` : error ? `: ${String(error)}` : ""
  console.error(`[browser-mcp] ${message}${suffix}`)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function send(socket: { send(data: string): void }, payload: unknown): boolean {
  try {
    socket.send(JSON.stringify(payload))
    return true
  } catch {
    return false
  }
}

/** Pasta das capturas: mesma do plugin do OpenCode, para os agentes reusarem. */
function captureDir(): string {
  const base = process.env.XDG_CACHE_HOME || join(homedir(), ".cache")
  return join(base, "opencode-browser", "capturas")
}

/**
 * Grava as capturas e devolve os caminhos. Uma falha de escrita não derruba a
 * tool: o texto da resposta apenas sai sem o caminho.
 */
function saveImages(images: ImagePayload[]): string[] {
  if (images.length === 0) return []
  const stamp = new Date().toISOString().replace(/[-:]/g, "").replace(/\..*$/, "").replace("T", "-")
  const paths: string[] = []
  try {
    mkdirSync(captureDir(), { recursive: true, mode: 0o700 })
    images.forEach((image, index) => {
      const ext = IMAGE_EXT[image.mime] ?? "bin"
      const path = join(captureDir(), `captura-${stamp}-${index + 1}.${ext}`)
      writeFileSync(path, Buffer.from(image.data, "base64"), { mode: 0o600 })
      paths.push(path)
    })
  } catch (error) {
    log("não foi possível gravar a captura em disco", error)
  }
  return paths
}

/** `browser_batch`: os itens trazem o nome sem prefixo na extensão. */
function normalizeArgs(tool: string, args: Record<string, unknown>): Record<string, unknown> {
  if (tool !== "browser_batch" || !Array.isArray(args.actions)) return args
  return {
    ...args,
    actions: args.actions.map((action) =>
      isRecord(action) && typeof action.name === "string"
        ? { ...action, name: action.name.replace(/^browser_/, "") }
        : action,
    ),
  }
}

async function waitForActive(ms: number): Promise<typeof state.active> {
  if (state.active) return state.active
  const deadline = Date.now() + ms
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 100))
    if (state.active) return state.active
  }
  return null
}

const NOT_CONNECTED =
  "A extensão OpenCode Browser não está conectada. Verifique se o Chrome está aberto com a extensão ativa e tente de novo."

function dispatch(
  socket: { send(data: string): void },
  id: string,
  tool: string,
  args: Record<string, unknown>,
  sessionId?: string,
): Promise<ToolResponse> {
  return new Promise<ToolResponse>((resolve, reject) => {
    const timer = setTimeout(() => {
      state.pending.delete(id)
      send(socket, { type: "cancel", id })
      reject(new Error(`A extensão não respondeu em ${CALL_TIMEOUT_MS / 1000}s.`))
    }, CALL_TIMEOUT_MS)
    state.pending.set(id, { socket, resolve, reject, timer })
    const sent = send(socket, {
      type: "tool_request",
      id,
      tool,
      args: normalizeArgs(tool, args),
      sessionID: sessionId,
    })
    if (!sent) {
      state.pending.delete(id)
      clearTimeout(timer)
      reject(new Error("Falha ao enviar o pedido para a extensão."))
    }
  })
}

/** Chama uma tool do navegador; devolve `{ ok:false, error }` em vez de lançar. */
async function callTool(tool: string, args: Record<string, unknown>): Promise<ToolResponse> {
  // Prefer the relay another process already runs (the OpenCode plugin's), so
  // this server never takes the extension away from it.
  if (remote) {
    try {
      const first = await callRemote(remote, tool, args)
      if (first.ok || !first.error?.startsWith("relay recusou o token")) return first
      // Stale token: re-read `relay.json` and retry once with the live one.
      const fresh = await refreshRemote(remote)
      if (!fresh) return first
      remote = fresh
      return await callRemote(fresh, tool, args)
    } catch (error) {
      return { ok: false, error: error instanceof Error ? error.message : String(error) }
    }
  }
  const socket = state.active ?? (await waitForActive(EXTENSION_GRACE_MS))
  if (!socket) return { ok: false, error: NOT_CONNECTED }
  const id = `mcp_${crypto.randomUUID()}`
  try {
    return await dispatch(socket, id, tool, args)
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) }
  }
}

function activate(socket: { send(data: string): void; data: SocketData }) {
  socket.data.authed = true
  if (state.active && state.active !== socket) {
    try {
      ;(state.active as unknown as { close?: (code: number, reason: string) => void }).close?.(
        1000,
        "Nova conexão da extensão",
      )
    } catch {
      // ignore
    }
  }
  state.active = socket
  send(socket, { type: "ready", v: 1, home: homedir() })
  log(`extensão conectada (porta ${state.port})`)
}

function startServer(port: number) {
  return Bun.serve<SocketData>({
    hostname: "127.0.0.1",
    port,
    fetch(request, bunServer) {
      const url = new URL(request.url)
      if (url.pathname === "/health") {
        return Response.json({ connected: Boolean(state.active), version: SERVER_VERSION })
      }
      if (url.pathname !== "/" && url.pathname !== "/ws") {
        return new Response("Not found", { status: 404 })
      }
      const origin = request.headers.get("origin") ?? ""
      if (!origin.startsWith("chrome-extension://")) {
        return new Response("Forbidden origin", { status: 403 })
      }
      if (!ALLOW_ANY_EXTENSION && origin !== `chrome-extension://${EXPECTED_EXTENSION_ID}`) {
        return new Response("Forbidden origin", { status: 403 })
      }
      const upgraded = bunServer.upgrade(request, {
        data: { authed: false, lastPong: Date.now() },
      })
      if (!upgraded) return new Response("WebSocket upgrade failed", { status: 400 })
      return undefined
    },
    websocket: {
      maxPayloadLength: MAX_WS_PAYLOAD,
      open(socket) {
        socket.data.lastPong = Date.now()
        socket.data.pingTimer = setInterval(() => {
          if (Date.now() - socket.data.lastPong > 60_000) {
            try {
              socket.close(4001, "extensão sem resposta")
            } catch {
              // ignore
            }
            return
          }
          send(socket, { type: "ping" })
        }, 20_000)
      },
      message(socket, raw) {
        if (typeof raw !== "string") return
        let message: Record<string, unknown>
        try {
          const parsed = JSON.parse(raw) as unknown
          if (!isRecord(parsed)) return
          message = parsed
        } catch {
          return
        }
        if (message.type === "hello") {
          const idOk = ALLOW_ANY_EXTENSION || message.extensionId === EXPECTED_EXTENSION_ID
          if (!idOk) {
            socket.close(1008, "Extensão não autorizada")
            return
          }
          if (!socket.data.authed) activate(socket)
          return
        }
        if (message.type === "pong") {
          socket.data.lastPong = Date.now()
          return
        }
        if (!socket.data.authed) return
        if (message.type === "tool_result" && typeof message.id === "string") {
          const item = state.pending.get(message.id)
          if (!item || item.socket !== socket) return
          state.pending.delete(message.id)
          clearTimeout(item.timer)
          if (message.ok === false) {
            item.reject(
              new Error(
                typeof message.error === "string" ? message.error : "A extensão retornou um erro.",
              ),
            )
            return
          }
          const images: ImagePayload[] = []
          if (Array.isArray(message.images)) {
            for (const item of message.images) {
              if (!isRecord(item)) continue
              const { name, mime, data } = item
              if (typeof data !== "string" || typeof mime !== "string") continue
              if (!IMAGE_MIMES.has(mime) || data.length > MAX_IMAGE_BASE64) continue
              images.push({ name: typeof name === "string" ? name : "imagem", mime, data })
              if (images.length >= 3) break
            }
          }
          item.resolve({
            ok: true,
            content: typeof message.content === "string" ? message.content : undefined,
            images: images.length ? images : undefined,
            metadata: isRecord(message.metadata) ? message.metadata : undefined,
          })
        }
      },
      close(socket) {
        if (socket.data.pingTimer) clearInterval(socket.data.pingTimer)
        if (state.active === socket) state.active = null
        for (const [id, item] of state.pending) {
          if (item.socket !== socket) continue
          clearTimeout(item.timer)
          state.pending.delete(id)
          item.reject(new Error("A extensão desconectou durante a execução."))
        }
      },
    },
  })
}

/** Sobe o relay numa porta livre, preferindo a primeira do candidato. */
function ensureServer() {
  for (const candidate of PORT_CANDIDATES) {
    try {
      const server = startServer(candidate)
      state.server = server
      state.port = candidate
      log(`relay ouvindo em ws://127.0.0.1:${candidate}`)
      return
    } catch {
      log(`porta ${candidate} indisponível`)
    }
  }
  throw new Error("não foi possível abrir o relay em nenhuma porta candidata")
}

// ---------------------------------------------------------------------------
// Relay de outro processo (HTTP)
//
// Se o plugin do OpenCode já está rodando, ele tem a extensão conectada. Subir
// um segundo relay numa porta maior roubaria a extensão (ela prefere a versão
// mais alta e, no empate, a porta mais alta), então quando `relay.json` aponta
// para um relay vivo falamos com ele por `POST /tool` — o mesmo caminho que o
// próprio plugin usa entre processos.
// ---------------------------------------------------------------------------

export type RemoteRelay = { port: number; token: string }

function relayDir(): string {
  const runtime = process.env.XDG_RUNTIME_DIR?.trim()
  if (runtime) return join(runtime, "opencode-browser")
  const cache = process.env.XDG_CACHE_HOME?.trim()
  return join(cache || join(homedir(), ".cache"), "opencode-browser")
}

/** Arquivo ou pasta regular (não symlink), do usuário atual e sem acesso a grupo/outros. */
function isTrusted(path: string): boolean {
  try {
    const stat = lstatSync(path)
    if (!stat.isFile() && !stat.isDirectory()) return false
    const uid = process.getuid?.()
    if (uid !== undefined && stat.uid !== uid) return false
    return (stat.mode & 0o077) === 0
  } catch {
    return false
  }
}

function readRelayInfo(): RemoteRelay | null {
  const file = join(relayDir(), "relay.json")
  try {
    if (!isTrusted(relayDir()) || !isTrusted(file)) return null
    const parsed = JSON.parse(readFileSync(file, "utf8")) as Partial<RemoteRelay>
    if (
      typeof parsed.token === "string" &&
      parsed.token &&
      typeof parsed.port === "number" &&
      Number.isInteger(parsed.port)
    ) {
      return { token: parsed.token, port: parsed.port }
    }
  } catch {
    // arquivo ausente ou inválido
  }
  return null
}

async function probeRelay(port: number): Promise<boolean> {
  try {
    const response = await fetch(`http://127.0.0.1:${port}/health`, {
      signal: AbortSignal.timeout(1_000),
    })
    if (!response.ok) return false
    const data = (await response.json()) as unknown
    return isRecord(data) && typeof data.version === "number" && "connected" in data
  } catch {
    return false
  }
}

let remote: RemoteRelay | null = null

/** Um relay vivo de outro processo, se houver. */
async function discoverRemote(): Promise<RemoteRelay | null> {
  const info = readRelayInfo()
  if (!info) return null
  if (!(await probeRelay(info.port))) return null
  log(`reutilizando o relay existente na porta ${info.port}`)
  return info
}

/**
 * Re-read `relay.json` after a failed call. The file is only read at boot, so
 * a relay that restarted (new token, same port) or a stale file left by an
 * earlier session would make every tool fail with `403 Forbidden` while the
 * MCP still looked healthy. A fresh, live relay with a different token is
 * picked up for the next call.
 */
async function refreshRemote(previous: RemoteRelay | null): Promise<RemoteRelay | null> {
  const info = readRelayInfo()
  if (!info) return null
  if (previous && info.token === previous.token && info.port === previous.port) return null
  if (!(await probeRelay(info.port))) return null
  log(`relay mudou (porta ${info.port}); usando o token atual`)
  return info
}

/** Chama a tool por HTTP no relay de outro processo. Exportada para os testes. */
export async function callRemote(relay: RemoteRelay, tool: string, args: Record<string, unknown>) {
  const response = await fetch(`http://127.0.0.1:${relay.port}/tool`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-relay-token": relay.token },
    body: JSON.stringify({ tool, args }),
    signal: AbortSignal.timeout(CALL_TIMEOUT_MS + 5_000),
  })
  // The other process rejected the token: its `relay.json` was rewritten (a
  // restart) or the file we read is from an older session. Surface a message
  // the user can act on instead of a bare `403`.
  if (response.status === 403) {
    return {
      ok: false as const,
      error:
        "relay recusou o token (403). O relay do OpenCode reiniciou e o relay.json mudou; " +
        "rode a tool de novo (o token é relido) ou reinicie o OpenCode. Se persistir, " +
        "apague o relay.json em $XDG_RUNTIME_DIR/opencode-browser/ e reabra o OpenCode.",
    }
  }
  // Any other failure still carries the real reason in the body — the OpenCode
  // relay answers `500 {"ok":false,"error":"Ref ref_1 not found."}` for a stale
  // ref, `500 {"ok":false,"error":"Only http(s) navigation is allowed."}` for
  // `about:blank`, and `503 {"ok":false,"error":NOT_CONNECTED}` with the
  // extension detached. Dropping the body leaves the caller with a bare status
  // code that says nothing about what to do next.
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as unknown
    const detail =
      isRecord(body) && typeof body.error === "string" && body.error ? `: ${body.error}` : ""
    return { ok: false as const, error: `relay respondeu HTTP ${response.status}${detail}` }
  }
  const data = (await response.json()) as unknown
  if (!isRecord(data)) throw new Error("resposta inválida do relay")
  if (data.ok === false) {
    return { ok: false as const, error: typeof data.error === "string" ? data.error : "falha no relay" }
  }
  const images = Array.isArray(data.images)
    ? (data.images as ImagePayload[]).filter(
        (image) =>
          isRecord(image) &&
          typeof image.data === "string" &&
          typeof image.mime === "string" &&
          IMAGE_MIMES.has(image.mime) &&
          image.data.length <= MAX_IMAGE_BASE64,
      )
    : undefined
  return {
    ok: true as const,
    content: typeof data.content === "string" ? data.content : undefined,
    images: images?.length ? images.slice(0, 3) : undefined,
    metadata: isRecord(data.metadata) ? data.metadata : undefined,
  }
}

// ---------------------------------------------------------------------------
// Tools expostas ao Claude Code (mesmo catálogo do plugin do OpenCode)
// ---------------------------------------------------------------------------

const tabId = { type: "number", description: "ID da aba dentro do grupo desta sessão." }

type BrowserTool = {
  name: string
  wire?: string
  description: string
  input: Record<string, unknown>
}

const tools: BrowserTool[] = [
  {
    name: "tabs_context",
    description:
      "Lista as abas do grupo desta sessão. Use antes de qualquer ferramenta que exija tabId. Se não houver grupo, use createIfEmpty.",
    input: {
      type: "object",
      properties: {
        createIfEmpty: { type: "boolean", description: "Cria um grupo/aba se ainda não existir." },
      },
      additionalProperties: false,
    },
  },
  {
    name: "tabs_create",
    description:
      "Abre uma URL no grupo desta sessão e devolve o tabId. Com a mesma origem (ou aba principal em branco) reaproveita a aba principal; com origem diferente abre uma nova aba.",
    input: {
      type: "object",
      properties: { url: { type: "string" }, active: { type: "boolean" } },
      additionalProperties: false,
    },
  },
  {
    name: "tabs_attach",
    description:
      "Move uma aba existente (informada por tabId ou a aba ativa do navegador) para o grupo desta sessão, dando ao agente acesso ao que já está aberto. Pede permissão. A aba sai do grupo atual do usuário.",
    input: { type: "object", properties: { tabId }, additionalProperties: false },
  },
  {
    name: "tabs_close_tab",
    wire: "tabs_close",
    description: "Fecha uma aba do grupo desta sessão.",
    input: {
      type: "object",
      properties: { tabId },
      required: ["tabId"],
      additionalProperties: false,
    },
  },
  {
    name: "navigate",
    description:
      "Navega para uma URL ou vai para trás/frente no histórico. Pede permissão para domínios novos.",
    input: {
      type: "object",
      properties: {
        url: {
          type: "string",
          description: "URL absoluta ou domínio; https:// é adicionado se faltar.",
        },
        direction: { type: "string", enum: ["back", "forward"] },
        tabId,
      },
      additionalProperties: false,
    },
  },
  {
    name: "read_page",
    description:
      "Retorna a árvore de acessibilidade da página com refs (ref_1, ref_2...). Use as refs em computer/form_input/find. Com frames=all, refs de iframes vêm prefixadas como f<frameId>_ref_N. Em páginas grandes use filter=interactive e max_chars.",
    input: {
      type: "object",
      properties: {
        filter: { type: "string", enum: ["interactive", "all"] },
        depth: { type: "number" },
        ref_id: { type: "string" },
        max_chars: { type: "number" },
        include_hidden: { type: "boolean" },
        frames: { type: "string", enum: ["main", "all"] },
        tabId,
      },
      additionalProperties: false,
    },
  },
  {
    name: "find",
    description:
      "Busca heurística por elementos usando texto/role/nome. Retorna até 20 refs. Prefira read_page filter=interactive para páginas grandes.",
    input: {
      type: "object",
      properties: {
        query: { type: "string" },
        max_results: { type: "number" },
        frames: { type: "string", enum: ["main", "all"] },
        tabId,
      },
      required: ["query"],
      additionalProperties: false,
    },
  },
  {
    name: "get_page_text",
    description:
      "Extrai o texto principal da página (artigo/main/body), sem HTML. Com frames=all, concatena também o texto dos iframes.",
    input: {
      type: "object",
      properties: {
        max_chars: { type: "number" },
        frames: { type: "string", enum: ["main", "all"] },
        tabId,
      },
      additionalProperties: false,
    },
  },
  {
    name: "form_input",
    description:
      "Preenche um campo identificado por ref (input, textarea, select ou contenteditable).",
    input: {
      type: "object",
      properties: { ref: { type: "string" }, value: { type: "string" }, tabId },
      required: ["ref", "value"],
      additionalProperties: false,
    },
  },
  {
    name: "computer",
    description:
      "Interage com a página via eventos confiáveis: click, type, key, scroll, hover, screenshot, zoom e wait. Use ref de read_page/find ou coordinate [x,y]. O screenshot também é gravado em disco e o caminho vem na resposta. Refs de iframe (f<id>_ref_N) funcionam para click/hover/scroll/type via eventos de DOM.",
    input: {
      type: "object",
      properties: {
        action: {
          type: "string",
          enum: ["click", "type", "key", "scroll", "hover", "screenshot", "zoom", "wait"],
        },
        ref: { type: "string" },
        coordinate: { type: "array", items: { type: "number" }, minItems: 2, maxItems: 2 },
        text: { type: "string" },
        keys: { type: "string", description: "Ex.: Enter, Tab, Ctrl+A." },
        submit: { type: "boolean" },
        click_count: { type: "number" },
        button: { type: "string", enum: ["left", "right", "middle"] },
        scroll_direction: { type: "string", enum: ["up", "down", "left", "right"] },
        scroll_amount: { type: "number" },
        duration: { type: "number", description: "Segundos para action=wait (máx. 10)." },
        region: { type: "array", items: { type: "number" }, minItems: 4, maxItems: 4 },
        tabId,
      },
      required: ["action"],
      additionalProperties: false,
    },
  },
  {
    name: "javascript_tool",
    description:
      "Executa JavaScript no contexto da página. Retorna o valor da última expressão. Último recurso, só para LER dados que as outras ferramentas não alcançam: nunca leia cookies/storage/tokens nem faça requisições a outros domínios. Pede permissão e corta a saída em 50 KB.",
    input: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["javascript_exec"] },
        text: { type: "string", description: "Código JavaScript." },
        tabId,
      },
      required: ["text"],
      additionalProperties: false,
    },
  },
  {
    name: "read_console_messages",
    description:
      "Lê mensagens de console/erros da aba. O monitoramento começa quando a ferramenta é usada; recarregue a página para capturar o carregamento.",
    input: {
      type: "object",
      properties: {
        pattern: { type: "string" },
        onlyErrors: { type: "boolean" },
        limit: { type: "number" },
        clear: { type: "boolean" },
        tabId,
      },
      additionalProperties: false,
    },
  },
  {
    name: "read_network_requests",
    description:
      "Lê requisições de rede (XHR/fetch/documentos) da aba. O monitoramento começa quando a ferramenta é usada.",
    input: {
      type: "object",
      properties: {
        urlPattern: { type: "string" },
        limit: { type: "number" },
        clear: { type: "boolean" },
        tabId,
      },
      additionalProperties: false,
    },
  },
  {
    name: "resize_window",
    description: "Redimensiona a janela da aba para testar layouts responsivos.",
    input: {
      type: "object",
      properties: { width: { type: "number" }, height: { type: "number" }, tabId },
      required: ["width", "height"],
      additionalProperties: false,
    },
  },
  {
    name: "update_plan",
    description:
      "Apresenta um plano de navegação (domínios + passos) para aprovação. Os domínios aprovados ficam liberados nesta sessão.",
    input: {
      type: "object",
      properties: {
        domains: { type: "array", items: { type: "string" } },
        approach: { type: "array", items: { type: "string" } },
      },
      required: ["domains", "approach"],
      additionalProperties: false,
    },
  },
  {
    name: "upload_image",
    description:
      "Envia arquivos (bytes em base64) para um input[type=file] ou área de drag & drop identificada por ref. Pede permissão. Máximo de 3 arquivos e ~5 MB cada.",
    input: {
      type: "object",
      properties: {
        ref: { type: "string", description: "Ref do input de arquivo ou do alvo de drop." },
        files: {
          type: "array",
          minItems: 1,
          maxItems: 3,
          items: {
            type: "object",
            properties: {
              data: { type: "string", description: "Bytes do arquivo em base64." },
              name: { type: "string" },
              mimeType: { type: "string" },
            },
            required: ["data", "name"],
            additionalProperties: false,
          },
        },
        tabId,
      },
      required: ["ref", "files"],
      additionalProperties: false,
    },
  },
  {
    name: "gif_creator",
    description:
      "Grava as ações do agente como quadros (screenshot por ação) e exporta um GIF animado. Use action=start antes de navegar/interagir, action=stop ao terminar e action=export para gerar o arquivo.",
    input: {
      type: "object",
      properties: { action: { type: "string", enum: ["start", "stop", "export"] } },
      required: ["action"],
      additionalProperties: false,
    },
  },
  {
    name: "batch",
    wire: "browser_batch",
    description:
      "Executa até 20 ações de navegador independentes em sequência para reduzir idas e voltas. Cada item é {name, input}, com o nome de uma ferramenta de navegador (ex.: read_page); ferramentas de código (read, glob, grep) não pertencem a este lote. Não use quando uma ação depende do resultado da anterior.",
    input: {
      type: "object",
      properties: {
        actions: {
          type: "array",
          minItems: 1,
          maxItems: 20,
          items: {
            type: "object",
            properties: { name: { type: "string" }, input: { type: "object" } },
            required: ["name", "input"],
            additionalProperties: false,
          },
        },
      },
      required: ["actions"],
      additionalProperties: false,
    },
  },
]

// ---------------------------------------------------------------------------
// MCP por stdio (JSON-RPC 2.0, uma mensagem por linha)
// ---------------------------------------------------------------------------

const PROTOCOL_VERSION = "2024-11-05"

function toolResult(response: ToolResponse) {
  if (!response.ok) {
    return {
      content: [{ type: "text", text: response.error ?? "(sem saída)" }],
      isError: true,
    }
  }
  const text = response.content?.trim() || "(sem saída)"
  const files = saveImages(response.images ?? [])
  const body = files.length ? `${text}\nArquivo: ${files.join(" ")}` : text
  const content: unknown[] = [{ type: "text", text: body }]
  // Bloco de imagem: o modelo vê a captura, não só o caminho.
  for (const image of (response.images ?? []).slice(0, 3)) {
    content.push({ type: "image", data: image.data, mimeType: image.mime })
  }
  return { content }
}

async function handleRpc(method: string, params: Record<string, unknown> | undefined) {
  switch (method) {
    case "initialize":
      return {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: { tools: {} },
        serverInfo: { name: "browser", version: "0.1.0" },
      }
    case "tools/list":
      return {
        tools: tools.map((tool) => ({
          name: tool.name,
          description: tool.description,
          inputSchema: tool.input,
        })),
      }
    case "tools/call": {
      const name = String(params?.name ?? "")
      const tool = tools.find((candidate) => candidate.name === name)
      if (!tool) throw new Error(`ferramenta desconhecida: ${name}`)
      const args = isRecord(params?.arguments) ? params.arguments : {}
      return toolResult(await callTool(tool.wire ?? tool.name, args))
    }
    case "ping":
      return {}
    default:
      throw new Error(`método não suportado: ${method}`)
  }
}

let buffer = ""

async function onLine(line: string) {
  const text = line.trim()
  if (!text) return
  let message: Record<string, unknown>
  try {
    const parsed = JSON.parse(text) as unknown
    if (!isRecord(parsed)) return
    message = parsed
  } catch {
    return
  }
  const { id, method, params } = message
  // Notificação (sem id): nada a responder.
  if (id === undefined || typeof method !== "string") return
  try {
    const result = await handleRpc(
      method,
      isRecord(params) ? (params as Record<string, unknown>) : undefined,
    )
    process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`)
  } catch (error) {
    process.stdout.write(
      `${JSON.stringify({
        jsonrpc: "2.0",
        id,
        error: { code: -32603, message: error instanceof Error ? error.message : String(error) },
      })}\n`,
    )
  }
}

function startStdio() {
  process.stdin.setEncoding("utf8")
  process.stdin.on("data", (chunk: string) => {
    buffer += chunk
    let index = buffer.indexOf("\n")
    while (index >= 0) {
      const line = buffer.slice(0, index)
      buffer = buffer.slice(index + 1)
      void onLine(line)
      index = buffer.indexOf("\n")
    }
  })

  process.stdin.on("end", () => {
    for (const [id, item] of state.pending) {
      clearTimeout(item.timer)
      state.pending.delete(id)
      item.reject(new Error("O servidor MCP encerrou."))
    }
    process.exit(0)
  })
}

async function main() {
  // Never steal the extension from a relay that already has it: reuse first,
  // and only listen when nobody else does.
  remote = await discoverRemote()
  if (!remote) ensureServer()
  startStdio()
  log(remote ? `MCP pronto via relay :${remote.port}` : "MCP pronto (stdin/stdout)")
}

// Under `bun test` the module is imported by the test runner: wiring stdin and
// a relay there would hijack the test process (the `end` handler alone exits
// it), so only boot when this file is the entry point.
if (import.meta.main) void main()
