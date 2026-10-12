#!/bin/sh
# Try molt in a throwaway Docker container, apart from the host: see "Try it in a container" in
# the README. Each run starts from a fresh sample project; the profile and config.toml stay on a
# named volume until `reset`.
set -eu

IMAGE=molt-sandbox
VOLUME=molt-sandbox-home
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

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
    set -f
    for name in ${MOLT_SANDBOX_ENV:-}; do
        # Only a name: `NAME=value` would put the value in the arguments
        case $name in
        '' | [0-9]* | *[!A-Za-z0-9_]*)
            echo "not a variable name in MOLT_SANDBOX_ENV: $name" >&2
            exit 2
            ;;
        esac
        set -- -e "$name" "$@"
    done
    set +f
    # A terminal only when there is one, so that `molt profile show` also works from a pipe
    if [ -t 0 ] && [ -t 1 ]; then
        set -- -t "$@"
    fi
    exec docker run --rm -i -v "$VOLUME:/molt" "$@"
}

[ $# -ge 1 ] || usage
command=$1
shift
case $command in
build)
    [ $# -eq 0 ] || usage
    # The toolchain of rust-toolchain.toml, so that the image runs the version molt is built with
    version=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$ROOT/rust-toolchain.toml")
    [ -n "$version" ] || {
        echo "no channel in $ROOT/rust-toolchain.toml" >&2
        exit 1
    }
    exec docker build -f "$ROOT/docker/Dockerfile" --build-arg RUST_VERSION="$version" \
        -t "$IMAGE" "$ROOT"
    ;;
config)
    [ $# -eq 1 ] || usage
    [ -f "$1" ] || {
        echo "no file at $1" >&2
        exit 1
    }
    # Through stdin, not a bind mount, which would make a missing path a directory on the host
    exec docker run --rm -i -v "$VOLUME:/molt" "$IMAGE" sh -c 'cat > /molt/config.toml' <"$1"
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
