# Plugins do Claude Code

Plugins do Claude Code que acompanham o gateway. Cada subpasta é um plugin
completo, pronto para `claude plugin validate` e para carregar com
`claude --plugin-dir <pasta>` (ou pelo marketplace abaixo). Um pode ser um mod
de hooks; outro, um servidor MCP.

| Plugin | O que faz |
|---|---|
| [`agent-usage`](agent-usage/README.md) | Cota do **OpenCode Go** na linha de status: janelas de 5h/semanal/mensal + créditos do console, lidos de `GET /usage` do gateway. |
| [`browser-relay`](browser-relay/README.md) | As abas do Chrome no Claude Code: as 18 tools da extensão OpenCode Browser (`navigate`, `read_page`, `computer`, `screenshot`...) como um servidor MCP. |

## Instalar

Na raiz do repositório:

```bash
./scripts/install-agents-plugin.sh            # checa /usage e instala (escopo user)
./scripts/install-agents-plugin.sh --browser  # + browser-relay (precisa de bun)
./scripts/install-agents-plugin.sh --dry-run  # mostra os comandos
./scripts/install-agents-plugin.sh --scope project
```

O script consulta `GET /usage` antes de instalar (404 = binário antigo, 401 =
token errado), lê o `auth_token` do `config.toml` e o repassa via
`--config auth_token=...`.

O repositório também é um marketplace (`.claude-plugin/marketplace.json`), então
a instalação manual equivalente é:

```
/plugin install agent-usage --marketplace marcos2872/opencode-claude-gateway
```

Para desenvolver, rode da pasta direto:

```bash
claude --plugin-dir plugins/agent-usage
claude plugin validate plugins/agent-usage
claude plugin test plugins/agent-usage
```

## Três formas de plugin, uma regra

- **`agent-usage`** para no contrato HTTP local (`/usage`): o gateway já resolve
  a credencial do plano, então o token nunca entra no ambiente do plugin e a
  linha funciona em qualquer superfície do Claude Code sem exigir `sqlite3`.
- **`browser-relay`** precisa de WebSocket e de um servidor HTTP, que um mod de
  hooks não tem — por isso é um plugin com **servidor MCP** próprio (Bun). Ele
  reaproveita o relay do plugin do OpenCode quando ele está rodando, em vez de
  disputar a extensão.

O critério: se a API da `$` resolve (HTTP, fs, clock), fica um mod de hooks; se
precisa de socket, processo ou bloco de imagem, vira plugin com MCP.

A API de plugins é **early access**: cada build regrava
`.claude-plugin/types/claude-code/index.d.ts` ao carregar o mod, e é esse
arquivo que vale como referência da API atual.
