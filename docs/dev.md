# Desenvolvimento

[← README](../README.md) · [Configuração](configuracao.md) · [CLI](config-cli.md) ·
[Desktop](config-desktop.md) · [Erros](erros.md) · **Dev** ·
[Arquitetura](arquitetura.md)

Pré-requisitos: Rust stable, `opencode` v2 logado (`opencode auth login`).

## Comandos

```bash
cargo test                    # testes unitários (tradução, aliases, config) + e2e (tests/gateway.rs, tests/pair.rs, tests/cache_usage.rs)
cargo test --test gateway     # só os testes e2e do gateway
cargo test --test pair        # pareamento v2 + catálogo/providers/credenciais
cargo test --test perf        # latência de tradução do proxy (report-only em debug)
cargo test <name>             # teste único por substring do nome
cargo llvm-cov --locked --all-targets --summary-only  # cobertura (requer llvm-tools-preview + cargo-llvm-cov)
cargo clippy -- -D warnings   # lint (precisa estar limpo)
cargo fmt --check             # formatação (precisa estar limpa)
./scripts/dev.sh              # servidor em modo dev (config/token/pair isolados)
```

## Modo dev (`scripts/dev.sh`)

Sobe o `ocg --serve` em foreground com um config gerado em
`target/dev/config.toml`, token de teste (`dev-secret`) e cache de pareamento em
`target/dev/ocg.pair` — não toca no config nem no token do seu uso normal.

```bash
./scripts/dev.sh                       # http://127.0.0.1:3939, auth dev-secret
./scripts/dev.sh --no-auth             # sem auth_token (aceita qualquer)
./scripts/dev.sh --offline             # opencode_pair=false (CLI + SQLite)
./scripts/dev.sh --port 3940 --fresh-pair
./scripts/dev.sh --model claude-opencode-go-glm-5-3-flash
./scripts/dev.sh --dry-run             # mostra config + comando e sai
```

Ele compila (use `--no-build` para pular), espera `/health` e imprime o
comando do Claude Code:

```bash
claude --settings target/dev/claude-settings.json
```

Esse arquivo tem `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN` e
`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`. O `--settings` tem precedência
sobre o `env` do `~/.claude/settings.json` — importante se o seu settings já
aponta para o backend direto do OpenCode. `Ctrl+C` derruba o servidor. Os
detalhes de teste de auth/modelos com `curl` estão em [Erros](erros.md) e no
`--help` do script.

### Config extra que sobrevive à regeneração

O script **regenera** `target/dev/config.toml` a cada execução, então edições
ali se perdem. O que precisa persistir vai em `target/dev/config.extra.toml`,
concatenado ao fim do config gerado:

```toml
# target/dev/config.extra.toml — apenas tabelas
[aliases."claude-sonnet-5"]
opencode = "opencode-go/glm-5.3-flash"
```

Só **tabelas** (`[aliases.*]`, `[tiers.*]`, `[disabled]`) são aceitas: chaves
de topo (`port`, `auth_token`, `default_model`) duplicariam as do config
gerado e o TOML falharia no boot.

Este checkout traz um extra de exemplo (ele não é versionado — `target/` está
no `.gitignore` — então vive só nesta máquina) com os ids de primeira parte que
o Claude Code manda por conta própria (`claude-sonnet-5`, `claude-opus-4-8`,
`claude-haiku-4-5`, …), cada um preso a um dos três modelos escolhidos
(`opencode-go/deepseek-v4.1-flash`, `opencode-go/muse-spark-1.3-contributor`,
`github-copilot/gpt-6-luna`). Sem isso, esses ids caem no match por id puro do
catálogo e vão para o `github-copilot/*` (WARN `ambiguous model id`) ou para o
free-tier `opencode/*` — veja [Blindando o Copilot](config-cli.md#blindando-o-copilot-contra-chamadas-de-fundo-ids-blindados)
e a receita completa em [Configuração](configuracao.md#prendendo-os-ids-de-primeira-parte-a-um-modelo-do-opencode-go).
Apague `target/dev/config.extra.toml` para voltar ao comportamento padrão.

Dois detalhes ao editar o extra:

- o `opencode` deve ser o **`modelID` real** do catálogo (`glm-5.3-flash`,
  com ponto, não `glm-5-3-flash`). Com a ref errada o alias aparece no
  `/v1/models` mas a resolução não acha a linha e volta silenciosamente ao
  match por id puro;
- um alias manual **consome** a ref (`used_refs` em `src/api/state.rs`), o que
  remove o alias automático `claude-opencode-go-*` daquele modelo do
  `/v1/models`. Re-declare o canônico no extra quando apontar um alias para o
  mesmo ref;

O `--dry-run` mostra o config efetivo e sinaliza quando o extra foi incluído.

## Landing page e documentação (`web/`)

O site é estático, sem build step, e é publicado como está pelo workflow
[`pages.yml`](../.github/workflows/pages.yml) em todo push na `main` que tocar
`web/**`. São duas áreas:

| Arquivo | Conteúdo |
|---|---|
| `web/index.html` | **apresentação**: hero, a tradução campo a campo e o CTA para a documentação |
| `web/docs.html` | **home da documentação**: índice de tópicos em cards + "o que é o ocg" |
| `web/docs-*.html` | uma página por tópico, todas com a mesma barra lateral (`<nav id="toc">`) |
| `web/styles.css` | tokens, layout (landing e `docs`), tabelas e breakpoints |
| `web/app.js` | tabs, botões de copiar, seletor de tradução, toggle/realce da sidebar |
| `web/assets/logo.svg` | logo/favicon (cópia para o artifact ser autocontido) |

Páginas de docs: `instalacao`, `daemon`, `config`, `auth`, `pareamento`,
`modelos`, `aliases`, `variantes`, `tiers`, `cli`, `subagentes`, `janela`,
`desktop`, `http`, `erros`, `arquitetura`.

Convenções ao editar:

- a **sidebar é duplicada** em cada `docs-*.html` (mesmo bloco `<nav id="toc">`).
  Ao adicionar um tópico, acrescente o link em todas as páginas — o `app.js`
  marca a página atual por nome de arquivo, então o href deve ser o arquivo, não
  um fragmento;
- o `index.html` é só apresentação: conteúdo de configuração vai para as páginas
  de docs, não para uma seção nova da landing;
- não há partials nem template engine. Se a duplicação da sidebar passar a
  incomodar, o caminho é um pequeno passo de build no workflow do Pages antes de
  publicar o artifact — hoje isso não é necessário.

Para pré-visualizar localmente:

```bash
cd web && python3 -m http.server 8000
# abra http://127.0.0.1:8000/
```

## CI e hooks

O workflow [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) roda em todo
push e em toda PR: `cargo fmt --all --check`, `cargo clippy --all-targets --
-D warnings` e `cargo test --all-targets`. Um job separado `perf` (após o `test`)
compila em release e força o orçamento de latência (ver abaixo). A branch `main`
está protegida: o status **test** é obrigatório e a branch precisa estar
atualizada — **uma PR não mergeia enquanto os testes não passarem** (o admin
ainda pode dar push direto).

Para reproduzir o CI localmente, os mesmos comandos acima com `--all-targets`.

Cada job publica um **resumo** ao final (painel *Summary* do GitHub Actions): o
`test` mostra o status de cada etapa (fmt/clippy/testes) e o total de testes; o
`perf` mostra a tabela p50/p95/etc. por cenário e o veredito do orçamento. Em
PRs, os resumos dos dois jobs também são publicados como **comentário sticky**
(por job, via [`.github/scripts/pr-sticky-comment.sh`](../.github/scripts/pr-sticky-comment.sh):
um único comentário marcado, atualizado a cada push — em PR de fork o token é
read-only e o comentário é pulado com warning). Os steps do `test` rodam com
`continue-on-error` e a falha é propagada no step final `Propagar falha`, então
o resumo/comentário sai completo mesmo quando fmt, clippy ou testes falham.

Há um hook de pre-commit em [`.githooks/pre-commit`](../.githooks/pre-commit) que
roda `fmt --check`, `clippy` e `test` antes de cada commit. Ele **não** é ativado
automaticamente (o Git não versiona `.git/hooks`); habilite uma vez por clone:

```bash
git config core.hooksPath .githooks
```

Para pular um commit específico: `git commit --no-verify`.

## Coverage

Medição com `cargo-llvm-cov` (requer `rustup component add llvm-tools-preview` +
`cargo install cargo-llvm-cov`, ambos fora do sandbox: os testes e2e fazem
`bind` em sockets `127.0.0.1`, bloqueado no sandbox):

```bash
cargo llvm-cov --locked --all-targets --summary-only   # tabela por arquivo + TOTAL no terminal
cargo llvm-cov --locked --all-targets --lcov --output-path lcov.info
```

Baseline em 2026-10-04 (`cargo-llvm-cov 0.9.1`, mesmo escopo do CI): **83,07%
linhas · 83,00% funções · 83,15% regiões** (164 testes passando). Maiores gaps:
`main.rs` 0% (entrypoint do binário), `daemon.rs` 20% (ciclo de vida do daemon),
`infra/upstream/estimate.rs` 23% (fallback local do `count_tokens`),
`autostart.rs` 44% e `infra/opencode.rs` 51% (dependem do binário/state real do
OpenCode). 100%: `domain/protocol.rs`, `infra/upstream.rs`,
`infra/upstream/heartbeat.rs`.

O job `coverage` do CI é **report-only** (não bloqueia merge): publica o TOTAL +
os 10 arquivos de menor cobertura no Summary e como comentário sticky na PR
(`<!-- ocg-ci-cov-summary -->`), e anexa o `lcov.info` como artifact.

## Performance

`tests/perf.rs` mede a **latência de tradução do proxy** no formato
Claude-simulador → gateway → OpenCode-simulador:

- O cliente é o próprio teste, via `axum_test::TestServer` in-process (o hop
  cliente→proxy fica **fora** da métrica).
- O upstream é um app axum mock num `TcpListener` real de loopback, então
  `reqwest`/serialização/pool de conexões entram na conta — é o caminho de
  produção. A suíte é uma matriz protocolo × formato de input (chat/responses/
  passthrough × minimal/realistic/imagem base64 ~512KB/gigante ~1MB/
  tools pesadas/multiturn/kitchen-sink/streaming, mais `count_tokens` local e
  via proxy) para nenhum formato de payload passar sem medição.
- Cada amostra cronometra a requisição inteira. No streaming, a métrica é o
  tempo total até drenar o SSE Anthropic visível ao cliente (~50 chunks
  enlatados por amostra no simulador). Após um warmup descartado (absorve o
  1º connect), reporta p50/p95/mean/min/max.

Enforcement: builds **release** exigem o orçamento p95 **por cenário** — 15ms
nos leves (minimal/realistic/passthrough/count via proxy), 25ms nos pesados
(imagem/gigante/tools/multiturn/kitchen-sink/streaming/`count_tokens` local),
que pagam parse/clone/serialize O(tamanho). O job `perf` do CI roda com
`--test-threads=1` para os cenários não disputarem CPU. **Labels de cenário
precisam ser `[a-z_]+`** (minúsculas + underscore): o grep do resumo não casa
outra coisa e a linha some da tabela silenciosamente. Builds **debug** —
como o `cargo test --all-targets` do job `test` — rodam e reportam, sem reprovar
(o timings sem otimização é ruidoso demais para gate). Para reproduzir ou ajustar:

```bash
cargo test --test perf -- --nocapture                                    # debug: só reporta
cargo test --release --test perf -- --nocapture --test-threads=1         # release: força os orçamentos
OCG_PERF_P95_MS=25 cargo test --release --test perf                    # sobrepõe TODOS os cenários
```

Para encarecer o cenário, aumente `FILLER_LINES` em `realistic_body`.

## Rodando

```bash
cargo run -- --refresh        # imprime catálogo: N modelos habilitados + aliases do gateway
RUST_LOG=warn cargo run -- --serve          # servidor em foreground na :3737
cargo run -- --serve --port 3739
```

O `--port` sobrescreve só a porta; o resto (`default_model`, aliases,
`[disabled]`, ...) continua vindo do `config.toml`, lido no boot.

Níveis de log — `trace` é o "all", do mais ao menos verboso:
`trace > debug > info > warn > error`:

```bash
RUST_LOG=trace cargo run -- --serve --port 3737   # tudo, incluindo hyper/reqwest/tokio
RUST_LOG=debug cargo run -- --serve --port 3737   # meio-termo, menos spam que trace
# só o projeto, sem o barulho das dependências:
RUST_LOG=opencode_claude_gateway=trace cargo run -- --serve --port 3737
RUST_LOG=opencode_claude_gateway=trace cargo run -- --serve --port 3737 2>&1 | tee ./ocg-dev.log
# projeto em trace, dependências em warn:
RUST_LOG=trace,hyper=warn,reqwest=warn,tokio=warn cargo run -- --serve --port 3737
```

Verificando o servidor:

```bash
curl -s http://127.0.0.1:3737/health
curl -s "http://127.0.0.1:3737/v1/models?limit=1000" | head -c 500
# fallback de requisição sem "model" (ver configuracao.md "Modelo padrão"):
curl -s http://127.0.0.1:3737/health | jq '{default_model, models}'
curl -s http://127.0.0.1:3737/v1/models | jq -r '.data[0].id'
```

## Resetando o cache do Claude Code (dev)

O discovery (`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`) busca
`GET /v1/models` a cada startup do Claude e grava em
`~/.claude/cache/gateway-models.json`. O catálogo do gateway é lido só no
boot, então após mudar aliases/código:

```bash
# 1. Gateway com catálogo fresco (mata o --serve antigo e sobe de novo)
curl -s http://127.0.0.1:3737/health  # confira "models" != 0
# daemon: ocg --stop && ocg --start
# foreground: cargo run -- --serve --port 3737

# 2. Confere o novo mapeamento (zero dup, fast com alias próprio)
curl -s http://127.0.0.1:3737/v1/models \
  | jq -r '.data[].id' | sort | uniq -d  # vazio = ok

# 3. Força o Claude a reler (saia do claude antes)
rm ~/.claude/cache/gateway-models.json
claude
# /model -> reselecione o id COM [1m], ex. claude-opencode-go-deepseek-v4-1-flash[1m]
```

Não apague `~/.claude/cache/model-catalog/` (catálogo publicado da
Anthropic, não do gateway). Se o picker mostra `Custom model`, o
`settings.json: model` tem a forma sem `[1m]` — funciona na API (o gateway
remove o sufixo ao resolver) mas o picker compara string exata; reselecionar
no `/model` reescreve o campo.

## Instalando o binário local

```bash
cargo install --path .
# depois `ocg` fica no PATH
```

## Publicando releases

As releases são publicadas automaticamente por uma **GitHub Action** sempre que
uma tag `v*` é criada:

```bash
git tag v0.1.0
git push origin v0.1.0   # a action builda e anexa o binário à release
```

## Daemon

```bash
ocg --start [--port 3737]   # pidfile ~/.local/share/opencode-claude-gateway/ocg.pid
ocg --status
ocg --stop
ocg --enable                # auto-start no login (systemd user service)
ocg --disable               # remove o auto-start
```

Logs: `~/.local/share/opencode-claude-gateway/ocg.log`.

## Estado de build

- `cargo clippy -- -D warnings` e `cargo fmt --check` precisam ficar limpos (como no CI).
- Os testes e2e (`tests/gateway.rs`, `tests/pair.rs`, `tests/cache_usage.rs`)
  usam um upstream mock — nunca chamam o binário real nem a rede. `tests/pair.rs`
  usa um shim de `opencode` e um servidor v2 mock em loopback.
- O model catalog vem do pareamento v2 (fallback `opencode api get /api/model`);
  os testes de servidor usam um `AppState` semeado e `opencode_pair = false`.

## Veja também

- [Arquitetura](arquitetura.md) — estrutura do código (domínio, infra, API).
- [Configuração do gateway](configuracao.md) — opções de `config.toml`.
- [Erros e diagnóstico](erros.md) — sintomas comuns e logs.
