#!/bin/sh
# Try molt in a throwaway Docker container, apart from the host: see "Try it in a container" in
# the README. Each run starts from a fresh sample project; the profile and config.toml stay on a
# named volume until `reset`.
set -eu

IMAGE=molt-sandbox
VOLUME=molt-sandbox-home
ROOT=$(cd "$(dirname "$0")/.." && pwd)

usage() {
    cat >&2 <<'USAGE'
usage: docker/sandbox.sh <command>

  build          build the image, with molt built inside it
  config PATH    copy a config.toml into the volume
  molt [ARGS…]   run molt in the sample project (e.g. `molt init`, `molt doctor`)
  shell          open a shell in the sample project
  reset          remove the volume (the profile and config.toml)

Environment passed by name, never by value: NEBIUS_API_KEY, LC_ALL, LC_MESSAGES, LANG, and the
names in MOLT_SANDBOX_ENV (space-separated), e.g. MOLT_SANDBOX_ENV=LOCAL_API_KEY for the
api_key_env of your config.toml.
USAGE
    exit 2
}

# Runs the image in a container that is removed when it exits, with a terminal and the volume.
run() {
    # `-e NAME` passes NAME's value only when it is set, and keeps the value out of the arguments
    set -- -e NEBIUS_API_KEY -e LC_ALL -e LC_MESSAGES -e LANG "$@"
    for name in ${MOLT_SANDBOX_ENV:-}; do
        set -- -e "$name" "$@"
    done
    exec docker run --rm -it -v "$VOLUME:/molt" "$@"
}

[ $# -ge 1 ] || usage
command=$1
shift
case $command in
build)
    [ $# -eq 0 ] || usage
    exec docker build -f "$ROOT/docker/Dockerfile" -t "$IMAGE" "$ROOT"
    ;;
config)
    [ $# -eq 1 ] || usage
    config=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
    exec docker run --rm -v "$VOLUME:/molt" -v "$config:/tmp/config.toml:ro" "$IMAGE" \
        cp /tmp/config.toml /molt/config.toml
    ;;
molt)
    run "$IMAGE" molt "$@"
    ;;
shell)
    [ $# -eq 0 ] || usage
    run "$IMAGE" bash
    ;;
reset)
    [ $# -eq 0 ] || usage
    exec docker volume rm "$VOLUME"
    ;;
*)
    usage
    ;;
esac
