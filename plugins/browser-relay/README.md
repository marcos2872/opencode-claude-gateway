# browser-relay (plugin Claude Code)

Usa as abas do Chrome pelo **Claude Code**, exatamente como o OpenCode faz: as
tools `mcp__browser__*` dirigem a extensão
[OpenCode Browser](https://github.com/marcos2872/opencode-browser-extension)
(navegar, ler a árvore de acessibilidade, clicar, digitar, screenshot,
console, rede, upload e GIF).

```
navigate, read_page, find, get_page_text, form_input, computer,
tabs_context, tabs_create, tabs_attach, tabs_close_tab, javascript_tool,
read_console_messages, read_network_requests, resize_window, update_plan,
upload_image, gif_creator, batch
```

## Por que um servidor MCP e não um mod de hooks

O plugin do OpenCode (`plugin/browser-relay.ts`) sobe um WebSocket onde a
extensão se conecta. Um mod de hooks do Claude Code não tem `WebSocket`, nem
`node:fs`, nem servidor HTTP — mas um plugin **pode embarcar um servidor MCP**
(`.mcp.json` + um processo Bun). Então:

- `server/browser-mcp.ts` fala MCP por stdio e conversa com a extensão (ou
  com o relay do OpenCode, se houver um).
- O protocolo com a extensão é o mesmo (`hello`/`tool_request`/`tool_result`),
  e a extensão descobre o relay sozinha por `GET /health` em 8799–8802.

## Não rouba a extensão do OpenCode

Se o plugin do OpenCode já está rodando, ele tem a extensão conectada. Subir um
segundo relay numa porta maior a roubaria (a extensão prefere a versão mais alta
e, no empate, a porta mais alta). Por isso o servidor **primeiro** lê
`relay.json` (`$XDG_RUNTIME_DIR/opencode-browser/`, 0600, checado por uid e
modo) e, se o relay estiver vivo, fala com ele por `POST /tool`. Só sobe um
relay próprio quando não há nenhum.

## Instalação

```bash
./scripts/install-agents-plugin.sh --browser      # assinatura do gateway + browser
```

ou, só este plugin:

```
/plugin install browser-relay --marketplace marcos2872/opencode-claude-gateway
```

Requisitos:

1. `bun` no `PATH` (o servidor MCP roda com `bun run`).
2. A extensão OpenCode Browser instalada e habilitada no Chrome.
3. Opcional: o OpenCode aberto — se estiver, o relay dele é reaproveitado.

## Capturas de tela

O screenshot vai para o modelo como **bloco de imagem** e também é gravado em
`~/.cache/opencode-browser/capturas/` (0600), com o caminho no texto. Uma falha
de escrita não derruba a tool.

## Configuração (variáveis de ambiente)

| Variável | Efeito |
|---|---|
| `OGB_MCP_PORT` | porta base do relay (padrão 8799) |
| `OPENCODE_BROWSER_EXTENSION_ID` | id da extensão autorizada |
| `OPENCODE_BROWSER_ALLOW_ANY_EXTENSION=1` | aceita qualquer `chrome-extension://` |

## Cuidados

- `javascript_tool` executa código na página; `update_plan` pede aprovação de
  domínios. A extensão é quem aplica as permissões — o Claude Code só transporta.
- O servidor escuta apenas em `127.0.0.1` e só aceita origem
  `chrome-extension://<id>`.
- MCP não permite cancelar uma chamada em andamento de fora; o relay corta em
  180 s.
