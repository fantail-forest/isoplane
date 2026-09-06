FROM mcr.microsoft.com/dotnet/sdk:10.0.400

RUN apt-get update && apt-get install -y --no-install-recommends clang zlib1g-dev curl xz-utils \
    && rm -rf /var/lib/apt/lists/*
ARG TARGETARCH
RUN case "$TARGETARCH" in arm64) arch=arm64 ;; amd64) arch=x86_64 ;; *) exit 1 ;; esac \
    && curl -fLsS "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-29/wasi-sdk-29.0-${arch}-linux.tar.gz" \
       | tar xz -C /opt \
    && ln -s /opt/wasi-sdk-29.0-${arch}-linux /opt/wasi-sdk
RUN case "$TARGETARCH" in arm64) arch=aarch64 ;; amd64) arch=x86_64 ;; *) exit 1 ;; esac \
    && curl -fLsS "https://github.com/bytecodealliance/wasm-tools/releases/download/v1.244.0/wasm-tools-1.244.0-${arch}-linux.tar.gz" \
       | tar xz -C /opt \
    && ln -s /opt/wasm-tools-1.244.0-${arch}-linux/wasm-tools /usr/local/bin/wasm-tools
ENV WASI_SDK_PATH=/opt/wasi-sdk \
    DOTNET_CLI_TELEMETRY_OPTOUT=1 \
    DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1 \
    DOTNET_NOLOGO=1 \
    MSBUILDDISABLENODEREUSE=1 \
    NUGET_PACKAGES=/nuget \
    HOME=/tmp
