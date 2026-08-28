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

# --- wasi-sdk (the C# guest's linker and PAL compiler) ----------------------
# NativeAOT-LLVM shells out to the WASI SDK's clang for the final link, and
# `guests/dotnet/build.sh` compiles `native/wisp_pal.c` with the same clang.
# The ILCompiler package pins an expected version and warns on anything else,
# so this is the version that package wants -- see guests/dotnet/README.md.
#
# It lives in the IMAGE rather than being fetched on first use, for the same
# reason .NET and wasm-tools do. `build.sh` used to install it itself into
# /opt, which cannot work: the container runs as the invoking uid (rule 1) and
# /opt is root-owned, so the untar failed with two hundred lines of
# "Cannot mkdir: Permission denied" and the build carried on to a confusing
# failure further down.
ARG WASI_SDK_VERSION=29.0
RUN curl -sSL -o /tmp/wasi-sdk.tar.gz \
      "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-${WASI_SDK_VERSION%%.*}/wasi-sdk-${WASI_SDK_VERSION}-x86_64-linux.tar.gz" \
    && tar xzf /tmp/wasi-sdk.tar.gz -C /opt \
    && rm /tmp/wasi-sdk.tar.gz \
    && /opt/wasi-sdk-${WASI_SDK_VERSION}-x86_64-linux/bin/clang --version | head -1
ENV WASI_SDK_PATH=/opt/wasi-sdk-29.0-x86_64-linux

# Cargo/NuGet caches and the container-side target dir are docker volumes.
# Docker creates a volume's mountpoint root:root when the path does not exist
# in the image, and sysbox then re-chowns volume contents into the container's
# uid range -- so a container running as the host user cannot write to them.
# Creating them here, owned by the invoking uid, is what makes
# `--user $(id -u):$(id -g)` actually usable. Without this the sandbox fails
# with "Permission denied" on .cargo-build-lock, which reads like a cargo bug.
ARG HOST_UID=1000
ARG HOST_GID=1000
RUN mkdir -p /ctarget /opt/cargo /opt/cargo/registry /nuget /tmp/home \
    && chown -R ${HOST_UID}:${HOST_GID} /ctarget /opt/cargo /nuget /tmp/home \
    && chmod -R u+rwX /ctarget /opt/cargo /nuget

WORKDIR /wisp
