#!/bin/sh
# Runs when Axiom.app is opened: registers axion at login (from inside the app), links the axiom CLI into
# ~/.local/bin, and uses models Ollama already downloaded unless AXIOM_ENGINE_* are set. Safe to rerun.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
quiet="${AXIOM_QUIET:-}" && unset AXIOM_QUIET # not a setting for axion
manifest="$HOME/.ollama/models/manifests/registry.ollama.ai/library"
if [ -z "${AXIOM_ENGINE_MODEL:-}" ] && [ -f "$manifest/qwen2.5/3b-instruct" ]; then
    export AXIOM_ENGINE_MODEL=ollama:qwen2.5:3b-instruct
fi
if [ -z "${AXIOM_ENGINE_EMBED:-}" ] && [ -f "$manifest/nomic-embed-text/latest" ]; then
    export AXIOM_ENGINE_EMBED=ollama:nomic-embed-text
fi
mkdir -p "$HOME/.local/bin" && ln -sf "$here/axiom" "$HOME/.local/bin/axiom"
if out=$("$here/axiom" daemon install 2>&1); then
    msg="axion now runs at login.\n\nCommand line: ~/.local/bin/axiom\nChat model: ${AXIOM_ENGINE_MODEL:-none (offline summaries)}\nEmbeddings: ${AXIOM_ENGINE_EMBED:-offline}\n\nRemove: axiom daemon uninstall"
else
    msg="Setup failed:\n$out"
fi
if [ -n "$quiet" ]; then
    printf '%b\n' "$msg"
else
    /usr/bin/osascript -e "display dialog \"$msg\" with title \"Axiom\" buttons {\"OK\"} default button 1" >/dev/null 2>&1 || printf '%b\n' "$msg"
fi
