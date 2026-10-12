# check=skip=InvalidDefaultArgInFrom
# A throwaway environment to try molt in, apart from the host: see "Try it in a container" in the README.
# Build and run it with docker/sandbox.sh.

# The channel of rust-toolchain.toml, which docker/sandbox.sh passes. It has no default, to keep
# one place for the version (hence the skipped check above)
ARG RUST_VERSION

FROM rust:${RUST_VERSION}-slim AS build
WORKDIR /src
COPY . .
# The caches keep the crates and their builds across builds, so a change to molt rebuilds only molt
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    CARGO_TARGET_DIR=/src/target cargo install --locked --path . --root /out

FROM rust:${RUST_VERSION}-slim
# molt runs cargo check, test, clippy and fmt, and git, in the project it works on
RUN rustup component add clippy rustfmt \
    && apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /out/bin/molt /usr/local/bin/molt
COPY examples/fake /opt/molt/examples/fake

RUN useradd --create-home molt \
    && mkdir /molt \
    && chown molt /molt
USER molt
RUN git config --global user.name molt-sandbox \
    && git config --global user.email molt-sandbox@localhost \
    && git config --global init.defaultBranch main \
    && mkdir -p /home/molt/work \
    && cd /home/molt/work \
    && cargo init --name sample sample \
    && cd sample \
    && git add . \
    && git commit -q -m "cargo init"

# The profile and config.toml, on the volume docker/sandbox.sh mounts here
ENV MOLT_HOME=/molt
WORKDIR /home/molt/work/sample
CMD ["bash"]
