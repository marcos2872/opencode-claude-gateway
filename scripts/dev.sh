#!/usr/bin/env bash
#
# ocg em modo dev: sobe o servidor em foreground com um config isolado,
# token de teste e cache de pareamento próprio em target/dev/.
#
# Uso:
#   ./scripts/dev.sh
#   ./scripts/dev.sh --port 3940 --no-auth
#   ./scripts/dev.sh --offline                 # CLI + SQLite, sem pair
#   ./scripts/dev.sh --fresh-pair --model claude-opencode-go-glm-5-3-flash
#   ./scripts/dev.sh --dry-run                 # mostra config/comando e sai
#
# O config gerado fica em target/dev/config.toml; edite-o e rode com
# --no-build para iterar mais rápido. Ctrl+C derruba o servidor.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PORT="${OCG_DEV_PORT:-3939}"
TOKEN="${OCG_DEV_TOKEN:-dev-secret}"
LOG_LEVEL="${OCG_DEV_LOG:-opencode_claude_gateway=debug}"
PAIR_ENABLED=1
OPENCODE_URL=""
DEFAULT_MODEL=""
WAIT=1
BUILD=1
FRESH_PAIR=0
DRY_RUN=0
PASSTHROUGH=()

usage() {
  cat <<'EOF'
Uso: scripts/dev.sh [opções] [-- <args extras para o ocg>]

  --port N          porta local (default 3939 / OCG_DEV_PORT)
  --token TOKEN     auth_token do gateway (default dev-secret / OCG_DEV_TOKEN)
  --no-auth         auth_token vazio (aceita qualquer credencial)
  --offline         opencode_pair = false (catálogo via CLI + SQLite)
  --url URL         opencode_url explícita (servidor v2 fora do padrão)
  --model ID        default_model do config de dev
  --log-level L     RUST_LOG (default "opencode_claude_gateway=debug")
  --fresh-pair      descarta o token de pareamento antes de subir
  --no-wait         não espera /health; sobe e mostra os logs
  --no-build        não roda `cargo build` (usa target/debug/ocg existente)
  --dry-run         só imprime o config e o comando
  -h, --help        esta ajuda

Variáveis: OCG_DEV_PORT, OCG_DEV_TOKEN, OCG_DEV_LOG.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --port) PORT="${2:?--port requer um valor}"; shift 2 ;;
    --token) TOKEN="${2:?--token requer um valor}"; shift 2 ;;
    --no-auth) TOKEN=""; shift ;;
    --offline) PAIR_ENABLED=0; shift ;;
    --url) OPENCODE_URL="${2:?--url requer um valor}"; shift 2 ;;
    --model) DEFAULT_MODEL="${2:?--model requer um valor}"; shift 2 ;;
    --log-level) LOG_LEVEL="${2:?--log-level requer um valor}"; shift 2 ;;
    --fresh-pair) FRESH_PAIR=1; shift ;;
    --no-wait) WAIT=0; shift ;;
    --no-build) BUILD=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    --) shift; PASSTHROUGH=("$@"); break ;;
    *) echo "opção desconhecida: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if ! [[ "$PORT" =~ ^[0-9]+$ ]] || (( PORT < 1 || PORT > 65535 )); then
  echo "porta inválida: $PORT" >&2
  exit 2
fi

toml_escape() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '%s' "$s"
}

STATE="$ROOT/target/dev"
CONFIG="$STATE/config.toml"
PAIR_FILE="$STATE/ocg.pair"
BIN="$ROOT/target/debug/ocg"
BASE="http://127.0.0.1:$PORT"

port_in_use() {
  if command -v ss >/dev/null 2>&1; then
    ss -ltn 2>/dev/null | awk '{print $4}' | grep -Eq "[:.]$PORT$"
  else
    (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null
  fi
}

if port_in_use; then
  echo "porta $PORT já está em uso; use --port para escolher outra" >&2
  exit 1
fi

mkdir -p "$STATE"
[[ "$FRESH_PAIR" -eq 1 ]] && rm -f "$PAIR_FILE"

{
  echo "# Gerado por scripts/dev.sh — rode o script de novo para regenerar."
  echo "port = $PORT"
  echo "auth_token = \"$(toml_escape "$TOKEN")\""
  echo "default_model = \"$(toml_escape "$DEFAULT_MODEL")\""
  echo "opencode_bin = \"opencode\""
  echo "opencode_pair = $([[ "$PAIR_ENABLED" -eq 1 ]] && echo true || echo false)"
  echo "opencode_url = \"$(toml_escape "$OPENCODE_URL")\""
} > "$CONFIG"

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "# config: $CONFIG"
  cat "$CONFIG"
  echo
  echo "# comando:"
  printf 'OCG_PAIR_SESSION=%q RUST_LOG=%q %q --serve --config %q --port %q' \
    "$PAIR_FILE" "$LOG_LEVEL" "$BIN" "$CONFIG" "$PORT"
  (( ${#PASSTHROUGH[@]} )) && printf ' %q' "${PASSTHROUGH[@]}"
  echo
  exit 0
fi

if [[ "$BUILD" -eq 1 ]]; then
  echo "› compilando ocg (debug)…"
  cargo build --quiet
fi
if [[ ! -x "$BIN" ]]; then
  echo "binário não encontrado: $BIN (rode sem --no-build)" >&2
  exit 1
fi

echo
echo "ocg dev · $BASE"
echo "  token:  ${TOKEN:-<desativado>}"
echo "  config: $CONFIG"
echo "  pair:   $([[ "$PAIR_ENABLED" -eq 1 ]] && echo "enabled ($PAIR_FILE)" || echo "offline (CLI + SQLite)")"
echo "  logs:   RUST_LOG=$LOG_LEVEL"
echo
if [[ -n "$TOKEN" ]]; then
  echo "  curl -s $BASE/health"
  echo "  curl -s -H 'x-api-key: $TOKEN' $BASE/v1/models | jq -r '.data[].id'"
  echo "  export ANTHROPIC_BASE_URL=$BASE"
  echo "  export ANTHROPIC_AUTH_TOKEN=$TOKEN"
else
  echo "  curl -s $BASE/health"
  echo "  curl -s $BASE/v1/models | jq -r '.data[].id'"
  echo "  export ANTHROPIC_BASE_URL=$BASE"
  echo "  export ANTHROPIC_AUTH_TOKEN=dummy"
fi
echo "  export CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1"
echo
echo "Ctrl+C para parar."
echo

OCG_PAIR_SESSION="$PAIR_FILE" RUST_LOG="$LOG_LEVEL" \
  "$BIN" --serve --config "$CONFIG" --port "$PORT" "${PASSTHROUGH[@]}" &
SERVER_PID=$!

cleanup() {
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
}
trap cleanup INT TERM EXIT

if [[ "$WAIT" -eq 1 ]]; then
  printf "› esperando /health"
  for _ in $(seq 1 60); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo
      echo "ocg saiu antes de responder /health" >&2
      wait "$SERVER_PID" || true
      exit 1
    fi
    body="$(curl -s -m 1 "$BASE/health" || true)"
    if [[ "$body" == *'"status":"ok"'* || "$body" == *'"status":"degraded"'* ]]; then
      echo
      echo "› $body"
      break
    fi
    printf "."
    sleep 0.5
  done
  echo
fi

wait "$SERVER_PID" || true
