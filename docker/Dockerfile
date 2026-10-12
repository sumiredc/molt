# A throwaway environment to try molt in, apart from the host: see "Try it in a container" in the README.
# Build and run it with docker/sandbox.sh.

# The version of rust-toolchain.toml, so that building molt does not fetch another toolchain
ARG RUST_VERSION=1.99.0

FROM rust:${RUST_VERSION}-slim AS build
WORKDIR /src
COPY . .
RUN cargo install --locked --path . --root /out

FROM rust:${RUST_VERSION}-slim
# molt runs cargo check, test, clippy and fmt, and git, in the project it works on
RUN rustup component add clippy rustfmt \
    && apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /out/bin/molt /usr/local/bin/molt
COPY examples/fake /opt/molt/examples/fake

# cargo writes its registry under CARGO_HOME when molt adds a dependency
RUN useradd --create-home molt \
    && chown -R molt /usr/local/cargo \
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
