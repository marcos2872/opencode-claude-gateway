# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, Codex, OpenCode, etc.) when working with code in this repository.

> Nota: este arquivo e `CLAUDE.md` devem ser mantidos em sincronia — mesmo conteudo, apenas o cabecalho difere.

## Commands

```bash
cargo test                    # unit + integration tests (tests/gateway.rs, tests/codex.rs)
cargo test --test gateway     # only the e2e gateway tests
cargo test --test perf        # proxy translation latency (report-only in debug)
cargo test <name>             # single test by name substring
cargo llvm-cov --locked --all-targets --summary-only  # cobertura (requer llvm-tools-preview + cargo-llvm-cov)
cargo clippy -- -D warnings   # must stay clean
cargo fmt --check             # must stay clean
cargo run -- --refresh        # print catalog + gateway aliases, exit
cargo run -- --serve          # foreground server on 127.0.0.1:3737
```

CI: `.github/workflows/ci.yml` runs `cargo fmt --all --check`, `cargo clippy
--all-targets -- -D warnings` and `cargo test --all-targets` on every push and
PR. Steps use `continue-on-error` + a final `Propagar falha` step so the job
summary and sticky PR comment (`.github/scripts/pr-sticky-comment.sh`) always
publish even when steps fail. A separate `perf` job builds `--release` and
enforces per-scenario p95 budgets (`tests/perf.rs`: protocol × input-shape
matrix, 15ms light / 25ms heavy; `OCG_PERF_P95_MS` overrides all; scenario
labels must stay `[a-z_]+` or the summary regex misses them). `main` is protected — the `test`
check is required and the branch must be up to date, so a PR cannot merge with
failing tests (admins may still push directly). An opt-in pre-commit hook
(`.githooks/pre-commit`, same three commands) is enabled per clone with
`git config core.hooksPath .githooks`.

Daemon lifecycle: `ocg --start [--port PORT] | --stop | --status`. Auto-start
on login is separate: `ocg --enable` / `--disable` install/remove a systemd
user service (`src/autostart.rs`, unit `~/.config/systemd/user/ocg.service`,
`ExecStart=<bin> --serve`); strict separation — they never start/stop a
running gateway and `--start`/`--stop` never touch auto-start. Configuration is loaded from `~/.config/opencode-claude-gateway/config.toml` (or `--config`/`OCG_CONFIG`), with `OCG_PORT` and `OCG_AUTH_TOKEN` overrides; first run at the
default path seeds a fully-commented `CONFIG_TEMPLATE` (`src/config.rs`, 0600). State files
(pid, port, session id, log) live in `~/.local/share/opencode-claude-gateway/` and are
created 0600 via `daemon::write_private`.

## Architecture

`opencode-claude-gateway` is a local Anthropic-compatible gateway that exposes OpenCode
v2's models to Claude Code. It listens only on `127.0.0.1` and never talks to
Anthropic — the upstream is the OpenCode Go/Console backend routed by provider
package.

Layers:

- **`domain/`** — pure types and rules, no async (`mod.rs` only re-exports).
  `model.rs` (`ModelRef`: provider/model, `#variant` suffix), `catalog.rs`
  (`CatalogEntry` deserializes `opencode api get /api/model` output, including
  `variants`, per-model headers/body defaults, context limits, and distinct
  catalog `id` rows), `protocol.rs` (`protocol_for_entry` → wire protocol,
  including `settings.endpoint` for mixed `github-copilot` catalogs),
  `alias.rs` (`auto_aliases_for` — gateway id must contain
  `claude`/`anthropic` for Claude Code's `/v1/models` discovery;
  `shield_cli_family_match` automatic family-spelling rewrite on advertised ids —
  `claude-sonnet`→`cs`, `claude-opus`→`co`, plus `haiku`/`fable`/`mythos` — via
  default-on `cli_shield_aliases`, so CLI background calls stop landing on those
  rows; optional `desktop_aliases` rewriting for Claude Desktop's denylist;
  `strip_window_suffix` for `[1m]`/`[200k]` hints Claude Code appends to unknown ids).
- **`infra/opencode.rs`** — OpenCode state. Credentials ONLY from the SQLite
  `credential` table (read-only; never `auth.json` as source of truth; never
  parse `opencode.jsonc`). Catalog via `opencode api get /api/model`. DB path
  via `opencode debug paths db`.
- **`infra/upstream/`** — pure translators (no HTTP; `upstream.rs` re-exports
  the public surface plus `join_url`):
  - `chat.rs`: `anthropic_to_openai` / `openai_to_anthropic` (Chat Completions)
  - `responses.rs`: `anthropic_to_responses` / `responses_to_anthropic` (Responses API).
    User-role items seen while `function_call`s await their outputs are held
    back and flushed after the outputs: the opencode-go backend rejects a
    mid-turn user injection sitting between pending calls and their results
    (`400 The request contains invalid parameters`). System items in the same
    spot are accepted and stay put.
  - `stream.rs`: `StreamTranslator` / `ResponsesTranslator` (SSE → Anthropic SSE)
  - `variant.rs`: `apply_variant` / `apply_variant_checked` — merge the selected variant into
    the **translated** body (translators drop unknown fields). Key per protocol:
    Chat = `reasoning_effort` (removed for `none`), Responses = `reasoning`
    object + `include`, Anthropic = `thinking` object (`adaptive`/`disabled`)
    from the variant's `thinking`/`effort`. A Messages-API variant with no
    representable field (`reasoningEffort` alone) is a 400, not a silent no-op.
  - `shared.rs` (crate-internal): `floor_output_tokens` — raises `max_tokens` /
    `max_output_tokens` below 16 to 16 on translated protocols (the zen backend
    rejects smaller output limits; Claude Code probes model switches with
    `max_tokens: 1`). The Anthropic passthrough path is untouched — plus the
    shared image/tool-result/body shaping used by both converters; catalog
    defaults (per-model `headers` and `body` fields, for example fast-mode
    beta headers and `speed`) are merged into forwarded requests at the
    `api/forward` layer.
  - `estimate.rs`: `estimate_tokens` — per-part local count for `count_tokens`
    (no tokenizer).
  - `heartbeat.rs`: `with_heartbeat` — injects `event: ping` during upstream
    silence; `sse` / `sse_error` — Anthropic mid-stream frames, `sse_error`
    used when the upstream stream fails or reports `response.failed` after
    opening; `responses_sse_error` — the `/v1/responses` edge's synthetic
    `response.failed` terminal frame (Codex ignores bare `error` events).
- **`api/server.rs`** — Axum handlers. `body` flows: resolve model → pick
  forward by `protocol_for_entry` (catalog `settings.endpoint` can override the
  package for mixed `github-copilot` rows) → translate → merge catalog defaults
  → apply variant → forward with `Bearer` credential + session headers
  (`x-opencode-session`, always sent). The reqwest client uses a short
  `connect_timeout` and a generous total `timeout` (`connect_timeout_secs` /
  `request_timeout_secs`), because the total also bounds streaming responses.
  Non-2xx upstream bodies are normalized to the Anthropic error shape
  (`upstream_error_response`), keeping the raw body only in the log — the
  `upstream rejected request` WARN also records `user_agent` and `session`
  (`x-claude-code-session-id`) so background callers can be attributed.
  Second client edge: `POST /v1/responses` (Codex, OpenAI Responses dialect)
  mirrors `messages()` minus `mock_classifier`, with OpenAI error shapes
  everywhere (including `require_token`'s 401, path-specialized). It forwards
  only `Protocol::Responses` entries via `forward_responses_passthrough`
  (body/SSE byte-for-byte + catalog variant/defaults + terminal-event claw:
  upstream truncation before `response.completed` emits a synthetic
  `response.failed` before EOF); Chat/Anthropic upstreams get
  Chat/Anthropic upstreams instead go through `forward_responses_translated`
  (Fase 2): canonical Anthropic request (`responses_to_anthropic_request`)
  → existing `anthropic_to_openai` for Chat rows → response reshaped back by
  `anthropic_to_responses_response` / `ResponsesOutTranslator`
  (Anthropic events → Responses SSE, custom tools round-trip as
  `custom_tool_call`; `reasoning`/`include`/exotic tools dropped — see
  docs/config-codex.md). Also under the flag:
  `GET /v1/models/codex` — the Codex-native catalog for the provider's
  `model_catalog_url` (`{"models":[...]}`, the only shape Codex's
  `ModelsResponse` decodes), every gateway alias, each carrying
  `base_instructions` (required by the Codex decoder; derived from Codex's
  bundled prompt, Apache-2.0, trimmed to harness sections — Codex caps the
  catalog download at 1 MiB and rejects the whole file above it, so the
  size is guarded by `codex_catalog_stays_under_the_client_download_cap`;
  asset in `src/api/codex_base_instructions.txt`).
  Gated by `config.responses_endpoint` (default true).

## Conventions & gotchas

- **Model resolution** (`AppState::resolve`): alias → `provider/model` → plain
  model id (ambiguous ids sort by qualified name, warn). Variants validated
  against the catalog; unknown label → 404 listing available ones. An empty
  `model` falls back to `config.default_model` (`effective_default`); with
  neither set it is a 400.
- **mock_classifier** (`config.toml`, default `false`): answers Claude Code's
  auto-mode safety-classifier checks and liveness probes locally, before
  `resolve` and only when `!stream` — real conversations (which carry tools)
  are never matched and still forward. Does NOT cover Claude Desktop's
  background title calls (different prompt, no `<block>`/`<severity>` tags).
- **`[tiers]`** (`config.toml`, default empty): Anthropic family tiers announced
  on `/v1/models` (`anthropic_family_tier` + `is_family_default`). Key = gateway
  id or `provider/model` ref. Desktop's `small_fast` background class picks the
  first `haiku`, then `sonnet`, then `opus` — unmapped aliases announce no tier.
  **Claude Code CLI ignores these**: its discovery cache schema strips to
  `{id, display_name, description}` + window, so it resolves background models
  by id-substring family spelling (which lands on the Copilot rows). The CLI is
  covered by the automatic `cli_shield_aliases` rewrite instead (default on;
  docs "Blindando o Copilot", `cs-`/`co-` ids) plus `ANTHROPIC_SMALL_FAST_MODEL`
  in `~/.claude/settings.json`. Prefer `[tiers]` keys by `provider/model` ref,
  which survive the automatic renames.
- **Free-tier**: `opencode/*` models 403 outside OpenCode, so they are hidden
  from `/v1/models` unless `include_free_tier = true`. They use the public key
  from `settings.apiKey` (`upstream_bearer`).
- **Session headers**: forwarding to Go always sends `x-opencode-session`
  (client's `x-claude-code-session-id`, then `x-opencode-session`, else a
  persisted fallback in `ocg.session`).
- **count_tokens**: Anthropic packages proxy to `{baseURL}/messages/count_tokens`
  with fallback to `estimate_tokens`; other packages always estimate locally.
  The proxy applies the resolved variant and the catalog body defaults so the
  count matches the real forward, and forwards the client's `anthropic-beta` /
  `anthropic-version` and session headers.
- **Error shape**: always `{"type":"error","error":{"type":...,"message":...}}`
  with Anthropic error types (`not_found_error`, `authentication_error`,
  `invalid_request_error`, `api_error`). Exception: everything on the
  `/v1/responses` Codex edge uses the OpenAI shape
  `{"error":{"message":...,"type":...,"code":...}}` — Codex cannot parse the
  Anthropic one (and its stream parser needs `response.completed`/`failed`
  before EOF, hence the terminal-event claw).
- **Codex edge** (`POST /v1/responses`, `responses_endpoint`, default `true`):
  Responses-upstream models pass through byte-for-byte (opencode-go/zen
  `/responses` rows + Copilot `endpoint: "responses"`); Chat/Anthropic
  upstreams use the Fase 2 canonical translation (all models work; the
  effort selector only reaches Responses upstreams — translated upstreams
  rely on the catalog variant). A model id that is neither a gateway alias
  nor an explicit `provider/model` ref falls back to `default_model`
  (`AppState::codex_model_or_default`) — Codex's bundled slugs like
  `gpt-6-luna` would otherwise fuzzy-match an arbitrary catalog row and burn
  Copilot quota; empty `default_model` keeps the old 404. Codex's
  `session-id`/`thread-id` headers feed `x-opencode-session` only after the
  Claude headers are checked, so the Claude path is byte-identical. Codex
  tests live in the self-contained `tests/codex.rs`; `tests/gateway.rs`
  stays Claude-only on purpose (compatibility canary). Codex's picker lists
  models via `GET /v1/models/codex` (`model_catalog_url` in
  `~/.codex/config.toml`); TOML gotcha: top-level `model`/`model_provider`
  keys must sit before the first `[table]` or they silently join that table.
- **Catalog is read once** in a background task after bind (no periodic refresh;
  remove/restart to pick up new models, `--refresh` previews what boot would
  load). `/health` stays `starting` until that first load, `degraded` after a
  failure, `ok` otherwise. Tests build a seeded `AppState` and never call the
  real binary.
- **Docs are split by audience under `docs/`** (Portuguese): `README.md` is only
  the project presentation + binary install + links; `docs/configuracao.md`
  (gateway `config.toml`, auth, aliases, variants), `docs/config-cli.md` (Claude
  Code CLI setup), `docs/config-desktop.md` (Claude Desktop + `[tiers]`),
  `docs/config-codex.md` (Codex via the `/v1/responses` edge),
  `docs/erros.md` (health/logs/troubleshooting), `docs/dev.md` (dev, CI,
  releases) and `docs/arquitetura.md` (layers, MVP limits). Keep them in sync
  when behavior changes, and preserve the top-of-file nav line and the
  "Veja também" cross-links between them.