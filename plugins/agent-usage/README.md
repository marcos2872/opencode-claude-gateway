# agent-usage (Claude Code mod)

Cota do **OpenCode Go** na linha acima do prompt do Claude Code: janela de 5
horas, semanal e mensal — cada uma com barra, porcentagem e o tempo até
resetar — mais os créditos pré-pagos do console. Atualiza sozinho a cada
minuto.

O mod não fala com o OpenCode nem lê `opencode.db`: ele consulta
`GET http://127.0.0.1:3737/usage` no **opencode-claude-gateway** local, que já
resolve a credencial do plano (pareamento v2 com fallback no SQLite) e faz o
cache das duas consultas upstream. O token do plano nunca passa pelo mod; só o
`auth_token` do gateway, e apenas nos headers da chamada local.

```
OpenCode Go 5h ░░░░░░░░░░░░░░   1%   wk ░░░░░░░░░░░░░░   3%   mo ████████░░░░░░  56%        US$ 0.66
               ↻ 3h53m                  ↻ 6d1h                  ↻ 8d20h
```

A barra é `█` (preenchido) e `░` (resto), sempre com 16 células no layout
largo. O `↻` de cada janela cai **sob a própria barra**, porque é o número
daquela barra. À direita ficam os créditos do console.

## Estados

| Situação | Como aparece |
|---|---|
| 0–49% | barra **verde** (`success`), porcentagem na cor normal |
| 50–89% | barra e porcentagem **âmbar** (`warning`) |
| ≥ 90% | barra e porcentagem **vermelhas** (`error`), com `!` depois do número |
| `rate-limited` | **vermelho** com ` limit` depois do número |
| última leitura falhou | o último valor continua na tela com ` stale` no fim |

As cores vêm do tema do Claude Code, então seguem claro/escuro e a paleta que
você usa. Na faixa o mod se chama `OpenCode Go`, porque ali não há prefixo do
engine.

Os vãos e o alinhamento são `Box` com `width` explícito, nunca `Text` de
espaços: um `Text` só de espaços não conserva as células quando o terminal
pinta, e era isso que colava as janelas umas nas outras e tirava o `↻` de baixo
da sua barra.

## Uso

- A faixa aparece sozinha e é **colapsável**. O `ocg install --agent-usage`
  grava o atalho `ctrl+x o` em `~/.claude/keybindings.json` (ação do engine
  `abovePrompt:toggle`, contexto `Chat`), sem remover o `ctrl+x ctrl+a` padrão —
  use `--move-keybinding` para ficar só com o novo, ou `--no-keybinding` para
  não mexer no arquivo. A ação vale para a faixa inteira, não só para este mod.
  A faixa também cede a vez a um questionário (`hasSurvey`) e volta depois.
- O plugin **não** pode registrar o chord sozinho: `plugin.json` não tem campo
  `keybindings` (o validador chama de campo desconhecido) e a API de mods só
  deixa um `Button` referenciar uma ação existente. Por isso o atalho é uma
  preferência do usuário, escrita pelo instalador.
- Os números **não** são repetidos na linha de status: ela só é usada para o
  que a faixa não consegue dizer (gateway fora do ar, 401, 404).
- A faixa se adapta à largura que sobra. Numa ordem de tentativas, cai o que não
  couber: barras de 16 → sem o cabeçalho → barras de 12 → sem a linha de reset
  → barras de 10 → barras de 8 com créditos/reset embaixo → só números. A
  primeira que couber inteira é a que desenha; nada é cortado no meio.
- `/agent-usage` força uma consulta e imprime o relatório completo (as três
  janelas com barra, reset, créditos e quando foi atualizado).
- Se o gateway falhar, a faixa mantém o último valor com ` stale`; o motivo
  aparece na linha de status e no `/agent-usage`.

## Instalação

Na raiz do repositório do gateway:

```bash
ocg install --agent-usage
```

O `ocg install` cria o config se preciso, fixa `base_url`/`auth_token` no
plugin e instala pelo marketplace. Sem o gateway instalado, o script antigo
continua valendo:

```bash
./scripts/install-agents-plugin.sh
```

Ele confere `GET /usage` antes de instalar e lê o `auth_token` do
`config.toml`. `--dry-run` mostra os comandos, `--plugin-dir` apenas imprime
como rodar sem instalar. Pelo marketplace, em uma linha:

```
/plugin install agent-usage --marketplace marcos2872/opencode-claude-gateway
```

## Requisitos

1. `ocg --serve` (ou `ocg --start`) rodando em `127.0.0.1:3737`.
2. `opencode auth login` feito para o plano Go (integração `opencode`).
3. Se o gateway tiver `auth_token` configurado, o token precisa ou estar na
   opção `auth_token` do mod, ou ser o `ANTHROPIC_AUTH_TOKEN` da sessão (é o que
   o `claude --settings` do `scripts/dev.sh` injeta). Sem nenhum dos dois o
   gateway responde `401` e a linha mostra `gateway 401 (set plugin auth_token)`.
   O fallback pelo ambiente vale **só para `base_url` em loopback**
   (`127.0.0.1`, `[::1]`, `localhost`): um `base_url` remoto nunca recebe o
   token da sessão — para ele, configure `auth_token` explicitamente.

## Configuração

As opções ficam no menu de configuração do plugin (`pluginConfigs` em
`~/.claude/settings.json`, chave `agent-usage`):

| Chave | Padrão | Descrição |
|---|---|---|
| `base_url` | `http://127.0.0.1:3737` | Onde o gateway escuta |
| `auth_token` | (vazio) | Valor de `auth_token` do `config.toml`; vazio usa o `ANTHROPIC_AUTH_TOKEN` da sessão quando `base_url` é loopback |

O token nunca é escrito no código do mod: ele vem da opção ou do ambiente da
sessão, e a comparação com o `auth_token` do gateway acontece no processo do
gateway (`src/api/server.rs`, `require_token`).

## Desenvolvimento

O mod é três arquivos (`plugin.json`, `hooks/hooks.json`, `hooks/register.tsx`)
mais o contrato de estado em `types/index.d.ts`. O layout da faixa é uma função
pura (`layout(payment, columns)` em `register.tsx`), então a apresentação é
testada sem UI:

```bash
claude plugin test plugins/agent-usage     # testes (inclui a faixa larga, média, estreita e o corte)
claude plugin validate plugins/agent-usage # contrato de hooks, tipos e escopo
```

**Duas âncoras de polling, de propósito.** A leitura da cota começa em
`plugin.register` (que dispara a cada load **e a cada reload**) e também em
`session.start`. Só a segunda existiria num mundo ideal, mas
`/reload-plugins` re-executa `register` sem disparar `session.start`: com a
âncora única, a faixa ficava em `sem dados ainda` para sempre depois de um
reload, com o gateway respondendo. A guarda `loaded` garante uma leitura e um
timer por ativação, não importa qual dispare primeiro.

O kit de teste (`claude plugin test`) ergue `session.start` mas não
`plugin.register` ($.plugin não existe nele), então o caminho do reload **não**
tem cobertura automatizada — ele é verificado na sessão real, com
`/reload-plugins`. Se a faixa aparecer preenchida logo depois de um reload, a
âncora funcionou.

Os testes de faixa montam o componente com `$.ui.mount({ component:
'AbovePrompt', props: { bodyColumns } })` e leem as linhas pelo `key`
(`row-0`, `row-1`, ...). A API de mods é early access, e
`.claude-plugin/types/claude-code/index.d.ts` é a autoridade da build atual.
