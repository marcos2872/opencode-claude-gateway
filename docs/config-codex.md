# Configuração no Codex

[← README](../README.md) · [Configuração](configuracao.md) · [CLI](config-cli.md) ·
[Desktop](config-desktop.md) · **Codex** · [Erros](erros.md) · [Dev](dev.md) ·
[Arquitetura](arquitetura.md)

Como apontar o **Codex** (Desktop/CLI) para o gateway. Enquanto Claude Code e
Claude Desktop falam o dialecto Anthropic (`POST /v1/messages`), o Codex só
fala a **OpenAI Responses API** (`POST {base_url}/responses`, sempre com
`stream: true`) — o gateway expõe essa porta como segunda borda de cliente.
As opções do lado do gateway estão em [Configuração](configuracao.md).

## Como funciona (Fase 1 — passthrough)

O handler `/v1/responses` recebe o corpo do Codex e o repassa **quase
intacto** ao upstream, mudando apenas o `model` (para o id do catálogo) e
mesclando a variante/defaults do modelo — mesma semântica dos outros caminhos:
a variante do catálogo sobrepõe o `reasoning.effort` pedido pelo cliente. A
resposta (JSON ou SSE) volta em bytes, com heartbeat local de 20s durante o
silêncio do upstream (o `event: ping` é ignorado com segurança pelo parser do
Codex) e uma **garra de evento terminal**: se o upstream cair ou truncar o
stream antes de `response.completed`, o gateway emite um `response.failed`
sintético antes de fechar (sem isso o Codex esperaria os 300s do idle timeout
dele).

Funciona hoje com os modelos cujo upstream já é a Responses API:

- linhas `opencode-go`/`zen` do endpoint `/responses` (grok-4.7/4.6,
  gpt-6-luna, gpt-5.6-luna, muse-spark);
- linhas Copilot de `endpoint: "responses"` (GPT-6/5.6, grok, mai-code, codex).

Modelos cujo upstream é Chat Completions (glm, kimi, deepseek, mimo, ...) ou
Messages (qwen, minimax) respondem **`501 not_implemented`** nesta porta — a
tradução para esses protocolos é a Fase 2 do plano. Erros (401, 404, 4xx do
upstream) saem sempre no **shape OpenAI** (`{"error":{"message","type","code"}}`),
que é o único que o Codex sabe ler.

## Lado do gateway

`~/.config/opencode-claude-gateway/config.toml`:

```toml
# Porta /v1/responses (default true; false desliga só esta borda)
# responses_endpoint = true
```

Auth igual à borda Anthropic: com `auth_token` setado, o Codex precisa mandar
`Authorization: Bearer <token>` (que é o que ele faz naturalmente — o
`env_key` abaixo cuida disso). Sem token configurado, qualquer chamada local é
aceita (default localhost-only).

## Lado do Codex

`~/.codex/config.toml`:

```toml
model = "claude-opencode-go-grok-4-7"   # id em GET /v1/models (copie o anunciado)
model_provider = "ocg"

[model_providers.ocg]
name = "opencode-claude-gateway"
base_url = "http://127.0.0.1:3737/v1"
env_key = "OCG_AUTH_TOKEN"              # só se auth_token estiver setado no gateway
wire_api = "responses"
```

`base_url` já inclui `/v1`: o Codex concatena `/responses` e bate em
`POST /v1/responses`. `wire_api = "responses"` é obrigatório (é o único
dialecto que ele fala para providers custom). Suba o gateway antes
(`ocg --start`) — e lembre do caveat de sempre: **o Codex Desktop não herda o
env do shell**, então `OCG_AUTH_TOKEN` precisa estar na sessão do app ou o
gateway rodar sem `auth_token` em localhost.

## Erros comuns

| Sintoma | Causa |
|---|---|
| `401 authentication_error` (shape OpenAI) | token errado/ausente — `env_key` ou `auth_token` do gateway. |
| `404 not_found_error` | id do modelo fora do catálogo — copie o id de `GET /v1/models`. |
| `501 not_implemented` (`code`) | modelo com upstream Chat/Anthropic — Fase 2 (use uma linha Responses). |
| `response.failed` no meio do stream | upstream caiu/truncou; a garra do gateway reporta `response.error.message`. |
| `403` de upstream `opencode/*` | free-tier fora do OpenCode (escondido no `/v1/models` a menos que `include_free_tier = true`). |

Detalhes de log em [Erros e diagnóstico](erros.md) (`upstream rejected
request` também registra `user_agent`/`session`, então dá para atribuir
chamadas de fundo do Codex).

## Fora de escopo (por ora)

- `count_tokens` e `mock_classifier` são específicos do Claude Code — o
  Codex não usa.
- WebSocket, `previous_response_id` e `store: true` não existem no
  transporte HTTP dele (`store: false` sempre).
- `GET /v1/models` não muda: o Codex não consulta para provider custom.

## Veja também

- [Configuração do gateway](configuracao.md) — `config.toml`, auth, aliases, variantes.
- [Configuração no Claude Code CLI](config-cli.md) — a borda Anthropic do mesmo gateway.
- [Configuração no Claude Desktop](config-desktop.md) — `[tiers]` e o picker do Desktop.
- [Erros e diagnóstico](erros.md) — saúde do gateway e tabela de sintomas.
