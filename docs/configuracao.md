# Configuração do gateway

[← README](../README.md) · **Configuração** · [CLI](config-cli.md) ·
[Desktop](config-desktop.md) · [Erros](erros.md) · [Dev](dev.md) ·
[Arquitetura](arquitetura.md)

Referência das opções do `config.toml`, dos overrides por variável de ambiente e de
como o gateway resolve modelos e aliases.

## Arquivo e overrides

Arquivo: `~/.config/opencode-claude-gateway/config.toml` (veja
[`config.example.toml`](../config.example.toml)).
**No primeiro run** o arquivo é criado automaticamente com todas as opções
comentadas (0600) — descomente e edite o que precisar. Paths informados via
`--config`/`OCG_CONFIG` não são criados (se não existir, valem os defaults).
Overrides por env: `OCG_PORT`, `OCG_AUTH_TOKEN`, `OCG_CONFIG`.
`OCG_AUTH_TOKEN` (quando não-vazio) sobrescreve o `auth_token` do arquivo.

O config é lido **no boot** — mudanças exigem reiniciar o gateway
(`ocg --stop && ocg --start`).

## Auto-start no login (`--enable` / `--disable`)

`ocg --enable` instala um systemd user service
(`~/.config/systemd/user/ocg.service`, `ExecStart=<bin> --serve`) e o habilita
para iniciar no login; `ocg --disable` remove tudo. Separação estrita: esses
comandos **não** sobem nem param o gateway — use `ocg --start` / `ocg --stop`
para o ciclo de vida. `ocg --status` mostra os dois estados.

O serviço roda direto o executável com `--serve`: porta e `auth_token` vêm
**do arquivo de config** (envs do shell, como `OCG_PORT`, não chegam ao
serviço). Com `--config` no momento do `--enable`, o caminho é gravado na unit.

```bash
ocg --enable                 # liga o auto-start (não sobe agora)
ocg --start                  # sobe agora (se quiser)
systemctl --user status ocg  # inspeciona o serviço
ocg --disable                # remove o auto-start (não para o gateway)
```

## Autenticação do gateway (`auth_token`)

Por padrão `auth_token = ""`: o gateway aceita qualquer credencial
(aceitável porque ele só escuta em `127.0.0.1`). Para exigir credencial:

```bash
# 1. Gere um token
openssl rand -hex 32

# 2. Salve no config do gateway (arquivo criado no primeiro run)
# edite ~/.config/opencode-claude-gateway/config.toml:
#   auth_token = "SEU_TOKEN_AQUI"

# 3. Reinicie o gateway para valer (o config é lido no boot)
ocg --stop
ocg --start   # ou --start --port XXXX se usa porta custom

# 4. Use o MESMO valor no Claude Code (ver docs/config-cli.md)
export ANTHROPIC_AUTH_TOKEN="SEU_TOKEN_AQUI"
```

Alternativa sem editar arquivo (teste / efêmero): exporte `OCG_AUTH_TOKEN`
antes do `--start` — ele sobrescreve o arquivo e é herdado pelo daemon filho.

Regras:

- Quando setado, todo endpoint exceto `GET /health` exige o token, via
  `x-api-key: <token>` **ou** `Authorization: Bearer <token>`.
- Token errado/ausente → `401 {"error":{"type":"authentication_error",...}}`.
- Trocar o token exige reiniciar (`--stop` + `--start`); só editar o
  arquivo não afeta o daemon já rodando.

## Modelo padrão (`default_model`)

Quando o cliente POSTa sem `"model"`, o gateway usa o `default_model` do
`~/.config/opencode-claude-gateway/config.toml` — **não** o `model` do
`~/.claude/settings.json` (um diz o que o gateway usa no fallback, o outro
o que o Claude pede). Vazio = primeiro alias em ordem alfabética, que hoje
costuma ser um `claude-github-copilot-...` (`g` < `o`), não o seu modelo de
chat. Fixe um que funciona:

```toml
default_model = "claude-opencode-go-muse-spark-1-3-contributor"
```

Reinicie o gateway (o config é lido no boot) e confira:

```bash
curl -s http://127.0.0.1:3737/health | jq '{default_model, models}'
curl -s http://127.0.0.1:3737/v1/models | jq -r '.data[0].id'
```

Se o `data[0].id` for um modelo do Copilot e o `default_model` estiver
vazio, qualquer chamada auxiliar sem `model` (probes do Claude, sem `tools`)
cai nele — é a origem dos `WARN upstream rejected request … 400
model_not_supported` "fantasmas" mesmo sem você selecionar o Copilot.
Veja [Erros](erros.md) para a distinção das origens.

## Timeouts do upstream

O cliente HTTP do gateway separa dois orçamentos (segundos):

- `connect_timeout_secs` (padrão `30`) — conexão TCP/TLS; faz um provider
  inalcançável falhar rápido.
- `request_timeout_secs` (padrão `3600`) — tempo total da requisição, **incluindo
  streaming**. É generoso de propósito: um turno longo com reasoning pode ficar
  aberto vários minutos. Reduza só se quiser cortar sessões presas.

## Mock do classificador do auto-mode (`mock_classifier`)

O Claude Code roda um classificador de segurança de dois estágios no auto-mode,
com ids auxiliares hardcoded (`claude-sonnet-5`, `claude-opus-4-8`) — além de
probes de disponibilidade de `max_tokens: 1`. Essas chamadas resolvem no catálogo
(e.g. a linha do GitHub Copilot) e, sem quota, geram WARN `upstream rejected
request … 429 quota exceeded` a cada verificação, gastando quota à toa.

Com `mock_classifier = true` o gateway responde essas verificações **localmente**
(nunca toca o upstream):

- estágio 1 → `<severity>0</severity>` (abaixo do limiar ⇒ libera sem estágio 2);
- estágio 2 → `<block>no</block>` (resposta começa com `<block>`, como o parser exige);
- probe de 1 token → `ok`.

A detecção lê só o `system` (marcadores `<severity>`/`<block>`) e exige `tools`
vazio — conversas reais (que carregam tools e o system prompt do Claude Code)
continuam encaminhadas normalmente, inclusive no modelo do Copilot selecionado.

> **Tradeoff:** com o mock ativo, a revisão de segurança do auto-mode sempre
> responde "allow" — equivalente a rodar o auto-mode sem o classificador LLM.
> As regras de permissão/hooks continuam valendo. Off por padrão.

> **Limite conhecido:** o mock só casa o classificador do Claude Code e seus
> probes. As chamadas de fundo do **Claude Desktop** (título da sessão, `max_tokens: 200`,
> sem `tools`) usam outro prompt e passam direto — para essas, use os tiers
> em [Configuração do Desktop](config-desktop.md#tiers-da-família-anthropic), não o mock.

## Pareamento com o OpenCode v2 (`opencode_pair`)

Por padrão o gateway pareia com o servidor OpenCode v2 selecionado
(`POST /api/pair` pelo CLI já autenticado, seguido de
`GET /auth/connect/:code` com `Accept: application/json`). O session token
resultante autentica as chamadas de catálogo, providers e credenciais com HTTP
Basic `opencode:<token>` e fica cacheado em
`~/.local/share/opencode-claude-gateway/ocg.pair` (0600). Em 401 o gateway
pareia de novo automaticamente.

```toml
# default: true — use a API v2 pareada
opencode_pair = true

# opcional: URL explícita. Vazio = descobre pelo serviço selecionado
# (~/.local/state/opencode/service.json ou `opencode service status`).
opencode_url = ""
```

Com `opencode_pair = false` (ou se o servidor estiver indisponível), o gateway
cai no modo offline: catálogo via `opencode api get /api/model` e credenciais
da tabela `credential` do SQLite (somente leitura). O mapeamento
provider → integração continua correto no modo pareado: por exemplo
`opencode-go/*` usa a credencial da integração `opencode`.

Se `opencode_url` apontar para um servidor diferente do serviço selecionado
(ex.: `opencode serve --standalone`), o CLI precisa da senha desse servidor:
exporte `OPENCODE_PASSWORD` no ambiente do gateway antes de subir.

## Cota do OpenCode Go (`GET /usage`)

O gateway expõe a cota do plano Go no mesmo listener local, para um mod do
Claude Code (ou a linha de status de qualquer cliente) acompanhá-la sem ler o
`opencode.db` nem ver o token:

```bash
curl -s -H "x-api-key: $ANTHROPIC_AUTH_TOKEN" http://127.0.0.1:3737/usage
```

```json
{
  "provider": "opencode-go",
  "windows": {
    "rolling": {"percent": 5.0, "status": "ok", "resets_at": "2026-11-17T02:24:38.000Z"},
    "weekly":  {"percent": 4.0, "status": "ok", "resets_at": "2026-11-23T00:00:00.000Z"},
    "monthly": {"percent": 41.0, "status": "ok", "resets_at": "2026-12-02T19:04:41.000Z"}
  },
  "credits": {"available_micro_cents": 364914398, "limit_micro_cents": null},
  "error": null,
  "status": null,
  "fetched_at": 1794872079
}
```

- **Fonte:** `GET https://opencode.ai/zen/go/v1/usage` (janelas de 5 horas,
  semanal e mensal) e `.../console/api/billing/status` (créditos pré-pagos,
  melhor esforço). A credencial é a da integração `opencode` — a mesma que o
  gateway já resolve pelo pareamento v2, com fallback no SQLite. Nenhum token
  aparece na resposta nem no log.
- **Cache:** 60 s no sucesso, 15 s no erro, compartilhado por todas as
  chamadas. Uma linha de status pode consultar a cada minuto sem sobrecarregar
  o console.
- **Falhas não são erro HTTP:** a resposta é sempre `200`, com `error` e
  `status` preenchidos e `windows`/`credits` nulos. Assim o cliente mantém o
  último valor conhecido em vez de apagar a linha. Um `401` normalmente pede
  `opencode auth login` de novo.
- **Créditos:** o console responde `availableMicroCents` como **string** e a
  unidade é 1e-8 de dólar (364914398 → US$ 3,65).
- **Autenticação:** como toda rota, exige o `auth_token` do gateway em
  `x-api-key` ou `Authorization: Bearer` quando ele está configurado.

O mod de referência (linha de status do Claude Code) fica fora deste
repositório; consulte o README dele para instalar.

## Escolhendo modelos — modelMap

Você escolhe modelos dentro do Claude Code via `/model`, alimentado por
`GET /v1/models`.

- **Automático:** cada modelo habilitado do OpenCode ganha um alias `claude-<provider>-<model>`
  (o prefixo `claude-` é obrigatório — a descoberta do Claude Code só mantém ids contendo
  `claude`/`anthropic`). Linhas com spelling de família first-party (`claude-sonnet-*`,
  `claude-opus-*`, mais `haiku`/`fable`/`mythos`) são blindadas por padrão
  (`cli_shield_aliases = true`): `claude-github-copilot-claude-sonnet-5` é anunciado como
  `claude-github-copilot-cs-5`, para as chamadas de fundo do CLI não caírem no Copilot —
  ver [CLI](config-cli.md#blindando-o-copilot-contra-chamadas-de-fundo-ids-blindados).
  Modelos >= 1M são anunciados como `claude-...[1m]` (único sufixo que
  o Claude lê); use essa forma com sufixo no `settings.json: model` para não aparecer como
  `Custom model` (sem sufixo funciona na API, o gateway remove ao resolver). Linhas com mesmo
  `provider/model` mas `id` distinto (ex. `opus-4.8` vs `opus-4.8-fast`) ganham aliases distintos.
  O catálogo é lido no boot com retry (veja [Erros](erros.md) para o caso `models:0`):
  para refletir modelos novos/removidos depois disso, reinicie o gateway (`--refresh` só
  pré-visualiza o que o boot carregaria). Para o picker do Claude Desktop (que descarta ids
  com nomes de modelos third-party), sete `desktop_aliases = true` — só o id anunciado muda
  (`...-deepseek-...` vira `...-d-eepseek-...`), refs e resolução intactos.
- **Manual:** `[aliases."<gateway-id>"]` no `config.toml` tem precedência sobre os automáticos;
  `[disabled]` esconde refs do picker **sem apagar a linha do catálogo** —
  o alias some do `/v1/models` e nunca é escolhido como default, mas a ref
  direta (`provider/model`) continua resolvendo, então dá para usar no chat
  explicitamente mesmo desabilitado. Com alias desabilitado, use a ref direta
  (`"model": "github-copilot/claude-haiku-4.5"`), não o alias antigo (vira 404).
  **Atenção:** `[disabled]` não é uma allowlist — o catálogo continua
  participando do match por **id puro**, então desabilitar
  `github-copilot/claude-sonnet-5` não impede uma request com
  `"model":"claude-sonnet-5"` de ir para ele (o WARN `ambiguous model id`
  continua). Para prender esse id a outro modelo, use um alias manual (veja
  [Prendendo os ids de primeira parte](#prendendo-os-ids-de-primeira-parte-a-um-modelo-do-opencode-go)).
- `POST /v1/messages` também aceita refs diretas (`opencode-go/kimi-k2.7-code`) e
  model ids simples, mesmo fora da lista.
- **Sem `"model"` na requisição** (probes/chamadas auxiliares): o gateway usa o
  `default_model` do config (ver [Modelo padrão](#modelo-padrão-default_model)), nunca
  "traduz" um modelo em outro — o `gateway_model` no log é sempre o id que o cliente pediu.

## Prendendo os ids de primeira parte a um modelo do opencode-go

O Claude Code manda, por conta própria, ids hardcoded de primeira parte:
side queries (`stream=true`, `max_tokens: 64000`, sem tools), small-fast,
o classificador do auto-mode e os fallbacks de família (`opus`/`sonnet`/
`haiku`). Esses ids **não** passam por `default_model` nem por `[tiers]` — eles
caem no match por **id puro** do catálogo, que escolhe a primeira linha
ordenada por `provider/model`:

- `claude-sonnet-5` → `github-copilot/claude-sonnet-5`, com o WARN
  `ambiguous model id, using first` (o par ambíguo é
  `[github-copilot/…, opencode/…]`);
- ids que só existem no free-tier (`claude-haiku-5-5`) → `opencode/*`, que dá
  403 fora do OpenCode.

Em ambos os casos você não escolheu aquele modelo nem aquele provider. Como um
**alias manual tem precedência** sobre o match por id puro, dá para prender
cada id a um modelo que você paga/quer:

```toml
[aliases."claude-sonnet-5"]
opencode = "opencode-go/glm-5.3-flash"
display_name = "Sonnet 5 -> GLM 5.3 Flash"
```

Regras que fazem a receita funcionar (todas verificadas contra o catálogo
real):

1. `opencode` é o **`modelID` exato** do catálogo — `glm-5.3-flash` (ponto),
   não o slug do alias (`glm-5-3-flash`). Com a ref errada o alias aparece no
   `/v1/models` mas a resolução não encontra a linha e volta ao id puro, com o
   mesmo WARN de antes.
2. Cubra **todos** os ids da família, não só os que aparecem no seu log hoje
   (`claude-opus-4-5` … `claude-opus-5-5`, `claude-fable-5*`): uma versão nova
   do CLI pode mandar outro id e a inferência volta.
3. Um alias manual **consome** a ref (`used_refs` em `src/api/state.rs`), o que
   remove o alias automático `claude-opencode-go-<model>` daquele modelo do
   `/v1/models`. Re-declare o canônico se quiser manter os dois:
   ```toml
   [aliases."claude-opencode-go-glm-5-3-flash"]
   opencode = "opencode-go/glm-5.3-flash"
   ```
4. O sufixo `[1m]` continua funcionando: o id é anunciado como
   `claude-sonnet-5[1m]` e o gateway remove o sufixo antes de resolver.

Verificação (o `upstream ok` loga o id pedido, então use `cache usage`, que
mostra o ref real):

```bash
RUST_LOG=opencode_claude_gateway=debug ocg --serve
curl -s -X POST -H 'x-api-key: TOKEN' -H 'content-type: application/json' \
  -d '{"model":"claude-sonnet-5","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}' \
  http://127.0.0.1:3737/v1/messages > /dev/null
# esperado no log:
#   upstream cache usage gateway_model=claude-sonnet-5 opencode_ref=opencode-go/glm-5.3-flash …
# e nenhum "ambiguous model id"
```

> **Limite:** isso é uma lista mantida à mão. Um id novo que o CLI passe a
> mandar volta a cair na inferência. Para proibir de vez, o `lookup_entry`
> precisaria recusar o match por id puro (um `strict_models`), o que ainda não
> existe.

### Variantes (`#variant`)

Modelos que declaram variantes no catálogo do OpenCode (ex.: reasoning effort)
aceitam o sufixo `#<variant>` no id — em aliases, refs diretas e ids simples
(`claude-opencode-go-my-model#high`). A variante é validada contra o catálogo
(404 com a lista de variantes disponíveis se não existir) e traduzida para o
parâmetro do wire protocol do upstream:

- **OpenAI-compatible** (`/chat/completions`): `reasoning_effort` (labels fora do
  enum da OpenAI, como `xhigh`/`max`, são saturados para `high`; `none` remove o
  parâmetro).
- **Responses API**: `reasoning` (objeto `{"effort": ...}`) mais os
  `include` declarados pela variante.
- **Passthrough Anthropic**: `thinking` derivado do que o catálogo declara para
  a variante — `{"type": "adaptive", "display": "summarized"}` para variantes
  com `thinking`, `{"type": "disabled"}` para `none`. Uma variante sem
  representação segura no Messages API (apenas `reasoningEffort`) retorna **400**
  em vez de ser silenciosamente ignorada.

## Veja também

- [Configuração no Claude Code CLI](config-cli.md) — env, `settings.json`, subagentes.
- [Configuração no Claude Desktop](config-desktop.md) — managed-settings, `[tiers]`.
- [Erros e diagnóstico](erros.md) — saúde do gateway e tabela de sintomas.
