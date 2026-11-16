#!/usr/bin/env bash
#
# Instala o mod `agent-usage` (cota do OpenCode Go na linha de status) no
# Claude Code, a partir da pasta plugins/ deste repositório.
#
# O mod consome `GET /usage` do gateway, então o passo que mais falha é o
# token: o `auth_token` do config.toml precisa ser o mesmo que o plugin recebe.
# Este script lê o config do gateway (e o OCG_AUTH_TOKEN) e o repassa como
# `--config auth_token=...`, além de checar /usage antes de instalar.
#
# Uso:
#   ./scripts/install-agents-plugin.sh
#   ./scripts/install-agents-plugin.sh --port 3939 --token dev-secret
#   ./scripts/install-agents-plugin.sh --scope project
#   ./scripts/install-agents-plugin.sh --plugin-dir      # só montar num dir local
#   ./scripts/install-agents-plugin.sh --dry-run         # mostra os comandos e sai
#
# Requisitos: `claude` no PATH e o gateway respondendo em /usage.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PLUGIN_DIR="$ROOT/plugins/agent-usage"
CONFIG="${OCG_CONFIG:-$HOME/.config/opencode-claude-gateway/config.toml}"
PORT="${OCG_PORT:-3737}"
TOKEN="${OCG_AUTH_TOKEN:-}"
SCOPE="user"
MODE="install"
DRY_RUN=0
SKIP_CHECK=0
FORCE=0

usage() {
  cat <<'EOF'
Uso: scripts/install-agents-plugin.sh [opções]

  --port N           porta do gateway (default 3737 / OCG_PORT)
  --token TOKEN      auth_token do gateway (default OCG_AUTH_TOKEN, senão lê o config)
  --config PATH      config.toml do gateway (default ~/.config/opencode-claude-gateway/config.toml)
  --scope SCOPE      escopo da instalação: user | project | local (default user)
  --plugin-dir       não instala: só imprime como rodar com `claude --plugin-dir`
  --no-check         não consulta /usage antes de instalar
  --force            não pede confirmação
  --dry-run          só imprime o que faria
  -h, --help         esta ajuda

Variáveis: OCG_PORT, OCG_AUTH_TOKEN, OCG_CONFIG.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --port) PORT="${2:?--port requer um valor}"; shift 2 ;;
    --token) TOKEN="${2:?--token requer um valor}"; shift 2 ;;
    --config) CONFIG="${2:?--config requer um valor}"; shift 2 ;;
    --scope) SCOPE="${2:?--scope requer um valor}"; shift 2 ;;
    --plugin-dir) MODE="plugin-dir"; shift ;;
    --no-check) SKIP_CHECK=1; shift ;;
    --force) FORCE=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "opção desconhecida: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$SCOPE" in
  user|project|local) ;;
  *) echo "escopo inválido: $SCOPE (use user, project ou local)" >&2; exit 2 ;;
esac
if ! [[ "$PORT" =~ ^[0-9]+$ ]] || (( PORT < 1 || PORT > 65535 )); then
  echo "porta inválida: $PORT" >&2
  exit 2
fi

BASE="http://127.0.0.1:$PORT"
MANIFEST="$PLUGIN_DIR/.claude-plugin/plugin.json"

if [[ ! -f "$MANIFEST" ]]; then
  echo "plugin não encontrado: $MANIFEST" >&2
  exit 1
fi

# -- token do gateway ------------------------------------------------------
#
# Precedência: --token > OCG_AUTH_TOKEN > auth_token do config.toml.
# O arquivo pode não existir (gateway nunca rodou) — aí vale token vazio.
if [[ -z "$TOKEN" && -f "$CONFIG" ]]; then
  TOKEN="$(
    sed -n 's/^[[:space:]]*auth_token[[:space:]]*=[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p' \
      "$CONFIG" | head -n1
  )"
fi

run() {
  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf '+ '
    printf '%q ' "$@"
    echo
  else
    "$@"
  fi
}

# -- pré-checagem do /usage ------------------------------------------------
#
# Instalar um mod que já começa mostrando 404/401 é pior do que não instalar:
# o `/usage` diz se o binário é novo e se o token bate antes de qualquer coisa.
check_usage() {
  if [[ "$SKIP_CHECK" -eq 1 ]]; then
    echo "› pré-checagem pulada (--no-check)"
    return 0
  fi
  if ! command -v curl >/dev/null 2>&1; then
    echo "› curl não encontrado; pulando pré-checagem"
    return 0
  fi
  local headers=()
  [[ -n "$TOKEN" ]] && headers=(-H "x-api-key: $TOKEN")
  local code body
  code="$(curl -s -o /tmp/ocg-usage-check.$$ -w '%{http_code}' -m 15 \
    "${headers[@]}" "$BASE/usage" || true)"
  body="$(cat /tmp/ocg-usage-check.$$ 2>/dev/null || true)"
  rm -f "/tmp/ocg-usage-check.$$"

  case "$code" in
    200)
      if [[ "$body" == *'"error":null'* ]]; then
        echo "› /usage OK em $BASE"
      else
        echo "› /usage respondeu, mas com erro de cota:"
        echo "    $(printf '%s' "$body" | head -c 240)"
        echo "  (o mod vai mostrar a linha com (stale); rode \`opencode auth login\` se for 401)"
      fi
      ;;
    000) echo "erro: gateway não respondeu em $BASE (suba com ./scripts/dev.sh ou ocg --serve)" >&2; return 1 ;;
    401) echo "erro: token rejeitado. Passe o auth_token certo com --token" >&2; return 1 ;;
    404) echo "erro: este gateway não tem GET /usage (binário antigo; recompile/atualize o ocg)" >&2; return 1 ;;
    *)   echo "erro: /usage respondeu HTTP $code" >&2; return 1 ;;
  esac
}

echo "agent-usage · $PLUGIN_DIR"
echo "  gateway: $BASE"
echo "  token:   ${TOKEN:-<vazio — gateway aceita qualquer credencial>}"
echo "  config:  $CONFIG"
echo

if ! command -v claude >/dev/null 2>&1 && [[ "$DRY_RUN" -eq 0 ]]; then
  echo "erro: \`claude\` não está no PATH" >&2
  exit 1
fi

if [[ "$MODE" == "plugin-dir" ]]; then
  echo "› modo --plugin-dir: nada é instalado; a sessão carrega a pasta direto."
  echo
  echo "  claude --plugin-dir $PLUGIN_DIR"
  echo
  echo "  valide/teste antes:"
  echo "    claude plugin validate $PLUGIN_DIR"
  echo "    claude plugin test $PLUGIN_DIR"
  exit 0
fi

check_usage

# -- instalação ------------------------------------------------------------
#
# O repositório é um marketplace (.claude-plugin/marketplace.json). Instalar
# pelo caminho local dá um vínculo com a pasta: editar o plugin e rodar
# /reload-plugins basta, sem reinstalar. `-y` aceita o comando declarado pelo
# marketplace (não há nenhum aqui) quando não há TTY.
if [[ "$FORCE" -eq 0 && "$DRY_RUN" -eq 0 ]]; then
  printf "Instalar agent-usage (escopo %s) a partir de %s? [s/N] " "$SCOPE" "$ROOT"
  read -r answer
  case "$answer" in
    s|S|y|Y) ;;
    *) echo "cancelado."; exit 0 ;;
  esac
fi

INSTALL_ARGS=(plugin install agent-usage --marketplace "$ROOT" --scope "$SCOPE" -y)
# The defaults in plugin.json already match a stock gateway; pass the values
# anyway so `/plugin configure` shows what this install actually uses.
INSTALL_ARGS+=(--config "base_url=$BASE")
if [[ -n "$TOKEN" ]]; then
  INSTALL_ARGS+=(--config "auth_token=$TOKEN")
fi

run claude "${INSTALL_ARGS[@]}"

if [[ "$DRY_RUN" -eq 0 ]]; then
  echo
  echo "› instalado. Na sessão atual rode /reload-plugins; novas sessões já carregam."
  echo "  a linha aparece sob o prompt (o Claude Code prefixa o nome do plugin):"
  echo "  5h 5% (resets in 2h14m) · wk 4% (resets in 6d3h) · mo 41% (resets in 15d19h) · US\$ 3.65"
  echo "  /agent-usage imprime o relatório completo."
fi
