# WISP proof-of-concept build & test environment.
# Intended to be run with `--runtime=sysbox-runc` so the prototype's use of
# mmap/madvise/cgroups/seccomp is exercised inside a properly sandboxed
# container rather than against the host kernel namespace.
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/opt/cargo \
    RUSTUP_HOME=/opt/rustup \
    PATH=/opt/cargo/bin:/usr/share/dotnet:/root/.dotnet/tools:$PATH \
    CARGO_TERM_COLOR=always

RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev ca-certificates curl git \
      cmake ninja-build python3 clang lld llvm \
      libzstd-dev libsqlite3-dev \
      procps iproute2 net-tools iputils-ping less jq time strace \
      util-linux zstd unzip xz-utils \
    && rm -rf /var/lib/apt/lists/*

# --- Rust (host runtime + Rust guests) -------------------------------------
ARG RUST_VERSION=stable
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain ${RUST_VERSION} --profile minimal \
    && rustup target add wasm32-unknown-unknown wasm32-wasip1 \
    && rustup component add rustfmt clippy

# --- .NET (C# guest experiment, §4.2 / G4) ---------------------------------
# Installed inside the image only; never on the host.
ARG DOTNET_CHANNEL=10.0
RUN curl -sSL https://dot.net/v1/dotnet-install.sh -o /tmp/dotnet-install.sh \
    && chmod +x /tmp/dotnet-install.sh \
    && /tmp/dotnet-install.sh --channel ${DOTNET_CHANNEL} --install-dir /usr/share/dotnet \
    && rm /tmp/dotnet-install.sh \
    && ln -sf /usr/share/dotnet/dotnet /usr/local/bin/dotnet \
    && dotnet --info | head -5
ENV DOTNET_CLI_TELEMETRY_OPTOUT=1 \
    DOTNET_NOLOGO=1 \
    DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1

# --- wasm tooling ----------------------------------------------------------
RUN cargo install --locked wasm-tools@1.244.0 || cargo install --locked wasm-tools

WORKDIR /wisp
