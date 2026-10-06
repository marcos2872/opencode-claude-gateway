# Configuração no Codex

[← README](../README.md) · [Configuração](configuracao.md) · [CLI](config-cli.md) ·
[Desktop](config-desktop.md) · **Codex** · [Erros](erros.md) · [Dev](dev.md) ·
[Arquitetura](arquitetura.md)

Como apontar o **Codex** (Desktop/CLI) para o gateway. Enquanto Claude Code e
Claude Desktop falam o dialecto Anthropic (`POST /v1/messages`), o Codex só
fala a **OpenAI Responses API** (`POST {base_url}/responses`, sempre com
`stream: true`) — o gateway expõe essa porta como segunda borda de cliente.
As opções do lado do gateway estão em [Configuração](configuracao.md).

## Como funciona

O handler `/v1/responses` escolhe o caminho pelo upstream do modelo:

- **Passthrough (Responses):** o corpo do Codex vai **quase intacto** ao
  upstream — só `model`, variante e defaults do catálogo mudam (a variante do
  catálogo sobrepõe o `reasoning.effort` do cliente) — e a resposta volta em
  bytes. Cobre as linhas `opencode-go`/`zen` do endpoint `/responses`
  (grok-4.7/4.6, gpt-6-luna, gpt-5.6-luna, muse-spark) e as Copilot de
  `endpoint: "responses"` (GPT-6/5.6, grok, mai-code, codex). Fidelidade
  total em `reasoning.encrypted_content`, `include` e tools.
- **Tradução (Chat/Anthropic):** o corpo vira uma request **canônica
  Anthropic** (`instructions` → `system`, `function_call`/`_output` →
  `tool_use`/`tool_result`, custom tools como função de campo `input`,
  imagens, `tool_choice`, `max_output_tokens` → `max_tokens`) e vai ao
  upstream no dialecto dele; a resposta (JSON ou SSE) é remontada para
  Responses. Cobre glm, kimi, deepseek, mimo (Chat) e qwen, minimax
  (Messages) — e todas as demais linhas do catálogo.

  Neste caminho **não sobrevivem** (por construção, documentado):
  `reasoning`/`include` (a variante do catálogo continua valendo), `store`,
  `prompt_cache_key`, `client_metadata`, `parallel_tool_calls` e tools sem
  equivalente Anthropic (`web_search`, `namespace` — aviso no log; tools
  `custom`/apply_patch **são** traduzidas).

Em ambos: heartbeat local de 20s durante o silêncio do upstream (o
`event: ping` é ignorado com segurança pelo parser do Codex) e uma **garra
de evento terminal** — se o stream terminar antes de
`response.completed`/`incomplete`/`failed`, o gateway emite um
`response.failed` sintético antes de fechar (sem isso o Codex esperaria os
300s do idle timeout dele). Erros (401, 404, 4xx do upstream) saem sempre no
**shape OpenAI** (`{"error":{"message","type","code"}}`), que é o único que o
Codex sabe ler.

### Modelo fora do catálogo → `default_model`

O Codex manda em algumas threads (ex.: fundos/automáticas) o slug embutido
dele — `gpt-6-luna` — que **não** é um alias do gateway. Sem guarda, o
resolve por id simples casaria com uma linha qualquer do catálogo
(`github-copilot/gpt-6-luna`, o primeiro em ordem) e a chamada ia bater no
Copilot com `429 quota exceeded` — um erro para um modelo que você nunca
escolheu. A edge Codex agora aplica: **modelo que não é alias nem
`provider/model` explícito → `default_model` da configuração** (com aviso no
log `codex edge: model not in gateway catalog`). Sem `default_model`
configurado, o comportamento antigo permanece (id desconhecido → 404).

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

### Passo a passo

1. **Suba o gateway**: `ocg --start` e confirme com
   `curl -s 127.0.0.1:3737/health` (espera `"status":"ok"`).
2. **Escolha um modelo**: pegue o slug do catálogo Codex-native
   (`GET /v1/models/codex`, ou `codex debug models` depois do passo 4) — ele
   vem **sem** o sufixo `[1m]` (ex.:
   `claude-opencode-go-d-eepseek-v4-1-flash`). É esse slug limpo que vai no
   `model`.
3. **Edite `~/.codex/config.toml`**: `model` e `model_provider` são chaves de
   **topo** — precisam vir **antes da primeira `[tabela]`**.
4. **Declare o provider** `[model_providers.ocg]` com `base_url`, `wire_api` e
   `model_catalog_url` (bloco abaixo).
5. **Reinicie o Codex** (ele relê o `config.toml` no boot) e confirme que o
   picker lista os modelos: `codex debug models`.

O resultado final de `~/.codex/config.toml` (chaves de topo no lugar certo,
provider no fim) — atenção: **`model` e `model_provider` precisam vir antes
da primeira `[tabela]`** (TOML: tudo depois de um header de tabela pertence
àquela tabela; colados no fim do arquivo, eles viram chaves de
`shell_environment_policy` e são silenciosamente ignorados):

```toml
# ~/.codex/config.toml
# ── 1) ESTAS DUAS LINHAS VÃO NO TOPO, ANTES DE QUALQUER [tabela] ──
model = "claude-opencode-go-d-eepseek-v4-1-flash"   # slug do picker, sem [1m]
model_provider = "ocg"

# ── 2) Suas outras tabelas ficam no meio, na ordem que já estão ──
#    [desktop], [mcp_servers.*], [plugins.*], ...

# ── 3) ESTE BLOCO É UMA TABELA PRÓPRIA; pode ficar em qualquer lugar depois ──
[model_providers.ocg]
name = "opencode-claude-gateway"
base_url = "http://127.0.0.1:3737/v1"
wire_api = "responses"
model_catalog_url = "http://127.0.0.1:3737/v1/models/codex"
# env_key = "OCG_AUTH_TOKEN"            # só quando auth_token estiver setado
```

Resumindo o "onde": as duas chaves de topo (`model`, `model_provider`) têm que
ser as **primeiras linhas do arquivo**; o bloco `[model_providers.ocg]` fica
depois das suas tabelas existentes, no fim do arquivo (ele já é uma tabela,
então pertence a si mesmo).

`base_url` já inclui `/v1`: o Codex concatena `/responses` e bate em
`POST /v1/responses`. `wire_api = "responses"` é obrigatório (é o único
dialecto que ele fala para providers custom). Caveat de auth: **o Codex
Desktop não herda o env do shell**, então com `auth_token` setado no gateway
o `OCG_AUTH_TOKEN` precisa estar na sessão do app (ou o gateway rodar sem
`auth_token` em localhost).

Sem `model_provider = "ocg"` no topo, o Codex **ignora o bloco
`[model_providers.ocg]` inteiro**: `base_url`, `wire_api` e
`model_catalog_url` não são usados, o picker mostra só os modelos built-in
(`gpt-6-luna`, `gpt-5.6-luna`, …) e `~/.codex/models_cache.json` nunca recebe
as aliases do gateway. Checklist rápido:

1. `model` e `model_provider` existem **antes da primeira `[tabela]`**?
2. `model` bate **exatamente** com um slug do catálogo (veja o picker ou
   `codex debug models`)?
3. O gateway responde em `127.0.0.1:3737` (`curl -s 127.0.0.1:3737/health`)?

## Modelos no picker (`model_catalog_url`)

O Codex **não lê `GET /v1/models`** de providers custom: sem um catálogo
nativo, o picker mostra só os modelos built-in (e a entrada do seu `model`
aparece como "modelo personalizado" sem nome). A chave `model_catalog_url`
do provider aponta para `GET /v1/models/codex`, que serve as mesmas aliases
em forma **Codex-native** (`{"models": [...]}` — o único formato que o parser
`ModelsResponse` decoda). **Todos** os modelos do gateway aparecem: os de
upstream Responses no passthrough, os demais pela tradução da Fase 2.
- Cada modelo carrega `base_instructions` (obrigatório para o decode do
  Codex): texto derivado das instruções bundled do próprio Codex
  (**Apache-2.0**, primeira frase neutralizada), **enxugado** para as
  seções de harness (regras de edição/`apply_patch`, ações destrutivas,
  autonomia) — o Codex limita o download do catálogo a **1 MiB** e
  recusa o arquivo inteiro acima disso (com o texto completo, 65 modelos
  estouravam o cap e o picker ficava vazio). O teste
  `codex_catalog_stays_under_the_client_download_cap` segura esse orçamento.
- `context_window` vai como campo estruturado, e **o slug não leva o sufixo
  de janela**: a borda Anthropic anuncia alguns ids com `[1m]`
  (`claude-opencode-go-d-eepseek-v4-1-flash[1m]`, que o Claude Code usa), mas
  o catálogo Codex os serve limpos. Configure `model` com o slug **sem**
  `[1m]` — o sufixo só existe para o Claude Code, e um `model` com `[1m]` não
  casa com nenhum slug do picker.
- Depois de mudar o config, reinicie o Codex (o catálogo é cacheado em
  `~/.codex/models_cache.json` com TTL de 5 min). Diagnóstico:
  `codex debug models` deve listar os modelos do gateway — e o cache
  (`count` + `identity`) confirma se a fetch pegou o catálogo certo: um
  `count` de ~6 slugs built-in (`gpt-6-luna`, `gpt-5.6-luna`, …) significa que
  o Codex caiu no catálogo nativo e o `model_provider` não está ativo.

## Erros comuns

| Sintoma | Causa |
|---|---|
| `401 authentication_error` (shape OpenAI) | token errado/ausente — `env_key` ou `auth_token` do gateway. |
| `404 not_found_error` | id do modelo fora do catálogo — copie o id de `GET /v1/models` (ou veja o picker com `model_catalog_url`). |
| picker só mostra os built-in (`gpt-6-luna`, …) | `model_provider = "ocg"` ausente ou não-topo — sem ele o Codex ignora `[model_providers.ocg]` e usa o catálogo nativo. |
| `model` não casa no picker | slug com sufixo `[1m]` — o catálogo Codex serve os ids **sem** o sufixo. |
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
