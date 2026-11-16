# agent-usage (Claude Code mod)

Cota do **OpenCode Go** na linha de status do Claude Code: janela de 5 horas,
semanal e mensal, mais os créditos pré-pagos do console, atualizados a cada
minuto.

O mod não fala com o OpenCode nem lê `opencode.db`: ele consulta
`GET http://127.0.0.1:3737/usage` no **opencode-claude-gateway** local, que já
resolve a credencial do plano (pareamento v2 com fallback no SQLite) e faz o
cache das duas consultas upstream. O token nunca entra no ambiente do mod.

```
5h 5% (resets in 2h14m) · wk 4% (resets in 6d3h) · mo 41% (resets in 15d19h) · US$ 3.65
```

O nome do plugin não é escrito no texto: o Claude Code já prefixa a linha de
status com ele. Uma janela acima de 90% ganha `!` e uma em `rate-limited`
ganha `limit` ao lado da própria porcentagem.

## Uso

- A linha aparece sozinha na sessão (`$.ui.status`), atualizada a cada 60 s,
  com as três janelas (5h, semanal, mensal), o reset de cada uma e os créditos.
- `/agent-usage` força uma consulta e imprime o relatório completo (as três
  janelas, créditos e quando foi atualizado).
- Se o gateway ou a cota falharem, a linha mantém o último valor com
  `(stale)` e o motivo vai no relatório do comando.

## Instalação

Na raiz do repositório do gateway:

```bash
./scripts/install-agents-plugin.sh
```

O script confere `GET /usage` antes de instalar, lê o `auth_token` do
`config.toml` e o repassa ao plugin (`--config auth_token=...`), e instala pelo
marketplace local. `--dry-run` mostra os comandos, `--plugin-dir` apenas
imprime como rodar sem instalar.

## Requisitos

1. `ocg --serve` (ou `ocg --start`) rodando em `127.0.0.1:3737`.
2. `opencode auth login` feito para o plano Go (integração `opencode`).
3. Se o gateway tiver `auth_token` configurado, informe o mesmo valor no mod.

## Configuração

As opções ficam no menu de configuração do plugin (`pluginConfigs` em
`~/.claude/settings.json`, chave `agent-usage`):

| Chave | Padrão | Descrição |
|---|---|---|
| `base_url` | `http://127.0.0.1:3737` | Onde o gateway escuta |
| `auth_token` | (vazio) | Valor de `auth_token` do `config.toml` |

## Desenvolvimento

O mod é três arquivos (`plugin.json`, `hooks/hooks.json`, `hooks/register.tsx`)
mais o contrato de estado em `types/index.d.ts`. Editar a pasta e rodar
`claude plugin validate .` recarrega e valida; a API de mods é early access, e
`.claude-plugin/types/claude-code/index.d.ts` é a autoridade da build atual.
