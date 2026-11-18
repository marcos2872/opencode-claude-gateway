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

## Não rouba a extensão do OpenCode — e não abre relays concorrentes

A extensão fala com **um** relay por vez. Subir um segundo relay numa porta
maior roubaria a extensão do primeiro (ela prefere a versão mais alta e, no
empate, a porta mais alta); subir numa porta livre deixa este relay
permanentemente sem a extensão, e toda tool responde "extensão não está
conectada" com o navegador aberto. Por isso:

- Ao subir, o servidor publica `relay.json` (`$XDG_RUNTIME_DIR/opencode-browser/`,
  0600, escrita atômica, checado por uid e modo) com o token e a porta.
- Antes de escutar, lê esse arquivo e, se o relay estiver vivo, fala com ele por
  `POST /tool` em vez de abrir um concorrente.
- Se a porta estiver ocupada por outro relay, reutiliza o dele quando o
  `relay.json` permite autenticar; se não permite (relay de sessão antiga), **não
  abre outro** quando esse relay tem a extensão — devolve um erro dizendo qual
  sessão encerrar.
- Quando o relay remoto morre, a sessão assume localmente em vez de falhar para
  sempre.


## Instalação

```bash
./scripts/install-agents-plugin.sh --browser      # assinatura do gateway + browser
```

ou, só este plugin:

```
/plugin install browser-relay --marketplace marcos2872/opencode-claude-gateway
```

Requisitos:

1. `bun` instalado (o servidor MCP roda com `bun run`). O script de instalação
   **para e pede para instalar o bun** quando não o encontra; para instalar:
   `curl -fsSL https://bun.sh/install | bash`.
2. A extensão OpenCode Browser instalada e habilitada no Chrome.
3. Opcional: o OpenCode aberto — se estiver, o relay dele é reaproveitado.

### `bun` não precisa estar no `PATH`

O caminho do `bun` é resolvido por `server/launch.sh`, e não pelo `.mcp.json`:
ele procura no `PATH` e, se não achar, em `$BUN_INSTALL/bin`, `~/.bun/bin`,
`/usr/local/bin`, `/opt/homebrew/bin` e `/usr/bin`. O `.mcp.json` chama
`/bin/sh <launch.sh>`, então nada depende de o bit de execução sobreviver ao
cache do plugin.

Isso existe porque `~/.bun/bin` normalmente entra no `PATH` pelo `~/.zshrc`, que
só é lido por um zsh **interativo**. Uma sessão do Claude Code iniciada por
launcher, systemd ou app gráfico herda um `PATH` mínimo e falhava com:

```
Connection failed (ENOENT): Executable not found in $PATH: "bun"
```

Se você vê esse erro numa sessão antiga, feche-a e abra outra: o PATH é lido
no boot do processo. O launcher já cobre os caminhos conhecidos, mas manter
`~/.zshrc` com `export BUN_INSTALL="$HOME/.bun"` e
`export PATH="$BUN_INSTALL/bin:$PATH"` evita surpresas com outras ferramentas.

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
- `POST /tool` exige `x-relay-token` (comparado com `timingSafeEqual`); sem o
  token do `relay.json` a resposta é 403. É o mesmo contrato do relay do
  OpenCode, então os dois podem se reaproveitar.
- MCP não permite cancelar uma chamada em andamento de fora; o relay corta em
  180 s.

## Diagnóstico: "extensão não está conectada" com o navegador aberto

Quase sempre é relay duplicado. Veja quem está escutando:

```bash
ss -ltnp | grep -E ':(8799|880[0-2])'
curl -s http://127.0.0.1:8799/health   # {"connected":true|false,...}
```

Se mais de um PID aparecer, cada sessão do Claude Code abriu o seu. Só uma tem
a extensão. Encerre as sessões extras; a que ficou com a porta 8799 e
`connected:true` é a dona. O log do MCP (`stderr`, visível no
`/mcp` ou nos logs do plugin) diz qual porta ele está reusando.
