#!/bin/sh
# Ponto de entrada do servidor MCP `browser`.
#
# Existe porque `bun` mora em ~/.bun/bin, um diretório que só entra no PATH por
# ~/.zshrc — e ~/.zshrc só é lido por zsh interativo. Uma sessão do Claude Code
# iniciada por launcher, systemd ou app gráfico herda um PATH mínimo e o MCP
# falhava com ENOENT antes de qualquer coisa: "Executable not found in $PATH:
# bun". Aqui o bun é resolvido pelo PATH e, se não estiver lá, pelos locais de
# instalação conhecidos; só quando nenhum existe é que o processo desiste, com
# instrução em vez de um ENOENT mudo.
#
# O `.mcp.json` chama este arquivo por `/bin/sh` (caminho absoluto), então nada
# aqui depende de PATH nem do bit de execução chegar intacto ao cache do plugin.

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
HOME=${HOME:-}

resolve_bun() {
  if command -v bun >/dev/null 2>&1; then
    command -v bun
    return 0
  fi
  for candidate in \
    "${BUN_INSTALL:-$HOME/.bun}/bin/bun" \
    "/usr/local/bin/bun" \
    "/opt/homebrew/bin/bun" \
    "/usr/bin/bun"
  do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

if ! BUN=$(resolve_bun); then
  cat >&2 <<'EOF'
[browser-mcp] bun não encontrado — o servidor MCP do navegador não vai subir.

Instale o bun e reabra a sessão do Claude Code:

  curl -fsSL https://bun.sh/install | bash

O bun fica em ~/.bun/bin. Para que qualquer processo (não só o terminal
interativo) o encontre, acrescente ao ~/.zshrc:

  export BUN_INSTALL="$HOME/.bun"
  export PATH="$BUN_INSTALL/bin:$PATH"

Este launcher já procura em ~/.bun/bin, então o plugin volta a funcionar mesmo
sem o PATH; as chamadas mcp__browser__* é que precisam de uma sessão nova.
EOF
  exit 127
fi

exec "$BUN" run --cwd "$ROOT" --shell=bun --silent server/browser-mcp.ts "$@"
