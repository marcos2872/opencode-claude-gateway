# Plugins do Claude Code

Mods (plugins de hooks) que acompanham o gateway. Cada subpasta é um plugin
completo, pronto para `claude plugin validate` e para carregar com
`claude --plugin-dir <pasta>` (ou pelo marketplace abaixo).

| Plugin | O que faz |
|---|---|
| [`agent-usage`](agent-usage/README.md) | Cota do **OpenCode Go** na linha de status: janelas de 5h/semanal/mensal + créditos do console, lidos de `GET /usage` do gateway. |

## Instalar

Na raiz do repositório:

```bash
./scripts/install-agents-plugin.sh            # checa /usage e instala (escopo user)
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

## Por que dentro do gateway

Os mods param no contrato HTTP local (`/v1/messages`, `/usage`) em vez de ler o
`opencode.db` ou falar com o console do OpenCode: o gateway já resolve e
cacheia a credencial do plano, então o token nunca entra no ambiente do
plugin, e a linha de status funciona em qualquer superfície do Claude Code
(terminal, Desktop, VS Code) sem exigir `sqlite3`.

A API de plugins é **early access**: cada build regrava
`.claude-plugin/types/claude-code/index.d.ts` ao carregar o mod, e é esse
arquivo que vale como referência da API atual.
