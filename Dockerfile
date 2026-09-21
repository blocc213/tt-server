# No `# syntax=` directive on purpose. That line makes BuildKit fetch
# docker.io/docker/dockerfile:<ver> before parsing, which fails on hosts that
# get 401/403 from Docker Hub. Nothing here needs a non-builtin frontend:
# no `COPY --link`, no `RUN --mount`, no heredocs.

# Frontend build: the browser bundle is produced at image-build time. Node.js is
# not present in the runtime image; tauritavern-server serves the built files.
FROM node:22-bookworm-slim AS frontend-builder

ENV PNPM_HOME=/pnpm \
    PATH=/pnpm:$PATH

WORKDIR /build
RUN corepack enable && corepack prepare pnpm@10.33.3 --activate

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile

COPY rspack.config.js tsconfig.host.json ./
COPY scripts ./scripts
COPY src ./src
RUN pnpm run web:build

# Rust build. The server crate deliberately has no Tauri/GTK/WebKit dependency;
# build-essential, cmake and pkg-config are only needed by native Rust crates
# such as tokenizers/onig/zstd during compilation.
FROM rust:1-bookworm AS rust-builder

WORKDIR /build

ENV CARGO_TARGET_DIR=/tmp/cargo-target
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        cmake \
        pkg-config \
        git \
    && rm -rf /var/lib/apt/lists/*

COPY src-tauri ./src-tauri

RUN cargo build \
      --locked \
      --release \
      --manifest-path src-tauri/Cargo.toml \
      -p tauritavern-server \
    && cp "$CARGO_TARGET_DIR/release/tauritavern-server" /tmp/tauritavern-server

# Runtime: Debian rather than scratch/distroless because tokenizer/native crates
# dynamically link the platform C++ runtime and libc. Node.js and the Rust build
# toolchain are not needed at runtime.
FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.title="TauriTavern Server" \
      org.opencontainers.image.description="TauriTavern browser server" \
      org.opencontainers.image.source="https://github.com/Darkatse/TauriTavern"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        gosu \
        libgcc-s1 \
        libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 tauritavern \
    && useradd --system --uid 10001 --gid tauritavern --home-dir /app tauritavern \
    && install -d -o tauritavern -g tauritavern /app /data /app/resources/frontend-templates

WORKDIR /app

COPY --from=rust-builder /tmp/tauritavern-server /usr/local/bin/tauritavern-server
COPY --from=frontend-builder /build/src /app/frontend
COPY default /app/resources/default
COPY src/scripts/templates /app/resources/frontend-templates
COPY scripts/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

RUN chmod 0755 /usr/local/bin/tauritavern-server /usr/local/bin/docker-entrypoint.sh \
    && chown -R tauritavern:tauritavern /app

VOLUME ["/data"]
EXPOSE 8000

ENV TAURITAVERN_PASSWORD="" \
    RUST_LOG="info"

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh", "/usr/local/bin/tauritavern-server"]
CMD ["--data-dir", "/data", "--frontend-dir", "/app/frontend", "--resources-dir", "/app/resources", "--host", "0.0.0.0", "--port", "8000"]
