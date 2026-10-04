<div align="center">
  <img src="assets/logo.svg" alt="opencode-claude-gateway" width="96" height="96">
</div>

# opencode-claude-gateway

Gateway local compatível com Anthropic que expõe seus **modelos do OpenCode** (v2) ao **Claude Code** — sem precisar de uma chave da Anthropic.

- Escuta **apenas em `127.0.0.1`** e nunca fala com a API da Anthropic: o upstream é o backend Go/Console do OpenCode.
- Reutiliza o **login do OpenCode** já existente na máquina (lê a credencial do SQLite, em memória).
- Traduz Anthropic Messages ↔ upstream (passthrough Anthropic, OpenAI Chat Completions ou Responses API, conforme o modelo).
- Roda como daemon em background com `--start` / `--status` / `--stop`; `--enable` / `--disable` ligam o auto-start no login (systemd user service).

```bash
ocg --start    # inicia em background
ocg --status
ocg --stop     # para
ocg --enable   # auto-start no login (opcional)
```

## Como funciona

| Assunto | Implementação |
|---|---|
| Credenciais (OpenCode v2) | Leitura somente do SQLite `opencode.db`, tabela `credential` (envelope `{"type","key"}` desembrulhado em memória, nunca logado). Caminho via `opencode debug paths db`, senão `OPENCODE_DB` / `XDG_DATA_HOME`. `auth.json` serve só como entrada de migração legada. |
| Catálogo de modelos | `opencode api get /api/model` (com service-auth), somente `enabled`. |
| Modelos de pacote Anthropic (MiniMax, Qwen…) | Passthrough de bytes para `{baseURL}/messages` (`anthropic-version`/`anthropic-beta` repassados). |
| Modelos OpenAI-compatible (Kimi, GLM, DeepSeek…) | Traduzidos para `{baseURL}/chat/completions` e de volta, incl. `tool_use`/`tool_result`, imagens, streaming SSE. |
| Modelos Responses-API (linhas Go GPT/Grok/Muse) | Traduzidos para `{baseURL}/responses` e de volta, incl. function calls e streaming. |
| Roteamento do OpenCode Go | Repassa o header nativo de sessão do Claude Code + sempre envia `x-opencode-session` (fallback estável persistido no data dir); User-Agent distintivo `ocg/x.y.z`. |

## Instalação (release)

Pré-requisitos:

- **OpenCode v2** instalado e logado (`opencode auth login`) — é quem autentica no Console e fornece o catálogo de modelos.
- `curl` e (opcionalmente) o `gh` CLI para baixar o binário.

Baixe o binário da **última release** e instale:

```bash
# 1. Obtenha a URL do binário mais recente
URL=$(gh release view --repo marcos2872/opencode-claude-gateway --json assets \
  --jq '.assets[] | select(.name=="ocg-linux-x86_64") | .url')
#    (sem gh instalado, copie o link direto da página da release)

# 2. Baixe, torne executável e mova para o PATH
curl -L "$URL" -o /tmp/ocg
install -m 0755 /tmp/ocg ~/.local/bin/ocg

# 3. Confira a versão
ocg --version
```

> Caminho alternativo: `cargo install --path .` ou
> `cargo install --git https://github.com/marcos2872/opencode-claude-gateway` (instalação
> a partir do código — veja [Desenvolvimento](docs/dev.md)).

No primeiro run o `~/.config/opencode-claude-gateway/config.toml` é criado
automaticamente com **todas as opções comentadas** — descomente e edite o que
precisar. Aponte seu cliente para o gateway:

- **Claude Code (CLI):** [Configuração no CLI](docs/config-cli.md)
- **Claude Desktop:** [Configuração no Desktop](docs/config-desktop.md)

### Migrando do `frank-opencode`

Troca limpa, sem fallback: pare o daemon antigo, mova o config e suba o novo.

```bash
frank-opencode --disable
mkdir -p ~/.config/opencode-claude-gateway
mv ~/.config/frank-opencode/config.toml ~/.config/opencode-claude-gateway/config.toml
ocg --start
```

(`FRANK_PORT`/`FRANK_CONFIG`/`FRANK_AUTH_TOKEN` viraram `OCG_*`; o state foi de
`~/.local/share/frank-opencode/` para `~/.local/share/opencode-claude-gateway/`.)

## Segurança

- Só escuta em `127.0.0.1`. DB aberto em `READ_ONLY`. Segredos ficam só em memória (`secrecy`), nunca logados.
- Arquivos de estado (`ocg.pid`, `ocg.port`, `ocg.session`, `ocg.log`) criados com `0600`.
- Quando `auth_token` está setado, todo endpoint exceto `/health` o exige
  (`x-api-key` ou `Authorization: Bearer`) — veja
  [Configuração → auth_token](docs/configuracao.md#autenticação-do-gateway-auth_token).

## Desenvolvimento

CI (GitHub Actions) roda formatação, lint e a suíte completa em todo push e PR.
A branch `main` está protegida: o status **test** é obrigatório e a branch precisa
estar atualizada — uma PR só mergeia com os testes verdes. Há também um hook de
pre-commit opcional. Como rodar em dev, testes, release e a suíte de latência:
[Desenvolvimento](docs/dev.md).

## Documentação

- [Configuração do gateway](docs/configuracao.md) — `config.toml`, auth, `default_model`, aliases, variantes.
- [Configuração no Claude Code CLI](docs/config-cli.md) — env, `settings.json`, subagentes, blindagem do Copilot.
- [Configuração no Claude Desktop](docs/config-desktop.md) — managed-settings, `desktop_aliases`, `[tiers]`.
- [Erros e diagnóstico](docs/erros.md) — `/health`, logs e tabela de sintomas.
- [Desenvolvimento](docs/dev.md) — como rodar em dev, testes, lint, releases.
- [Arquitetura](docs/arquitetura.md) — como o gateway é estruturado (domínio, infra, API).
