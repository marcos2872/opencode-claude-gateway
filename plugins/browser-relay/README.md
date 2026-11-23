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

## Sintoma: MCP sobe, mas as tools dão «Forbidden» (403)

O MCP inicializa normalmente — ele fala stdio e não depende do relay para
subir — e mesmo assim `mcp__browser__*` falha. Um `403` nesse caminho é sempre
o relay de **outro processo** recusando o token, nunca a extensão.

Causas, em ordem de frequência:

1. **`relay.json` velho.** O token do relay do OpenCode é regravado a cada
   sessão; um arquivo de uma sessão anterior (ou de outra máquina, quando
   `XDG_RUNTIME_DIR` não está setado e o fallback `~/.cache` viaja junto)
   aponta para um token que o relay vivo não aceita mais.
2. **Relay reiniciado no mesmo porto.** Novo token, mesma porta: o arquivo
   mudou depois de o MCP ler.
3. **Dois usuários/ambientes** com `XDG_RUNTIME_DIR` diferentes: o MCP lê um
   arquivo, o relay vive com outro.

Correção automática (deste plugin): ao receber `403`, o servidor relê o
`relay.json` e tenta **uma vez** de novo com o token atual; se o arquivo
estiver parado no mesmo token, responde com a instrução abaixo em vez de um
`403` cru. Se persistir, feche o OpenCode (que remove o `relay.json` ao sair),
apague o arquivo à mão e reabra:

```bash
rm -f "${XDG_RUNTIME_DIR:-$HOME/.cache}/opencode-browser/relay.json"
```

Depois reinicie a sessão do Claude Code (`/reload-plugins` basta para recarregar
o MCP). Verificação rápida: `curl -s http://127.0.0.1:8799/health` deve
mostrar `{"connected":true,...}`; o token do arquivo tem que ser o mesmo que o
relay aceita.

## Outros erros: `relay respondeu HTTP 4xx/5xx: <motivo>`

Fora do `403` de token, o relay do OpenCode responde os erros de uma tool com
status `500` e um corpo `{"ok":false,"error":"<motivo>"}`. O servidor repassa
esse `error` junto do status, então a mensagem que chega ao modelo já diz o que
aconteceu:

| Mensagem | Causa |
|---|---|
| `HTTP 500: Ref ref_3 not found.` | a ref veio de uma leitura anterior e a página já navegou (ou a extensão recriou a árvore). Rode `read_page`/`find` de novo e use a ref nova. |
| `HTTP 500: Only http(s) navigation is allowed.` | `navigate` para `about:blank` ou `file://`. Use uma URL `http(s)`. |
| `HTTP 503: <extensão não conectada>` | a extensão não está aberta/conectada no Chrome. |
| `HTTP 500: <timeout>` | a extensão não respondeu em 180 s; a chamada foi cancelada. |

Versões anteriores a esta mostravam só `relay respondeu HTTP 500`, sem o motivo.

## Testes

```bash
cd plugins/browser-relay/server && bun test
```

O servidor só sobe quando o arquivo é o ponto de entrada (`import.meta.main`),
então o `bun test` importa `browser-mcp.ts` sem abrir relay nem tocar no stdin.

## Cuidados

- `javascript_tool` executa código na página; `update_plan` pede aprovação de
  domínios. A extensão é quem aplica as permissões — o Claude Code só transporta.
- O servidor escuta apenas em `127.0.0.1` e só aceita origem
  `chrome-extension://<id>`.
- MCP não permite cancelar uma chamada em andamento de fora; o relay corta em
  180 s.
