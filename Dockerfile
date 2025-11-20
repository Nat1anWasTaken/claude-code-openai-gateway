# syntax=docker/dockerfile:1.7

### Build stage ###
FROM rust:1-bookworm AS builder

WORKDIR /app

# Install minimal build deps then clean apt caches.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates pkg-config build-essential \
    && rm -rf /var/lib/apt/lists/*

# The project uses Rust 2024 edition; use nightly until stabilized in stable images.
RUN rustup toolchain install nightly && rustup default nightly

# Leverage layer caching for dependencies.
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release

### Runtime stage ###
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates nodejs \
    && rm -rf /var/lib/apt/lists/*

# Treat binaries in /usr/local/bin as ES modules so the mounted `claude` script (ESM)
# executes without "Cannot use import statement outside a module".
RUN printf '{ "type": "module" }\n' > /usr/local/bin/package.json

# App user keeps runtime minimal and non-root.
RUN useradd -m app
WORKDIR /home/app

COPY --from=builder /app/target/release/claude-code-openai-gateway /usr/local/bin/claude-code-openai-gateway

USER app
EXPOSE 8080
ENV RUST_LOG=info

# NOTE: The container expects the `claude` CLI to be available in PATH at runtime.
# You can bake it into a derivative image or mount it at /usr/local/bin/claude.
CMD ["claude-code-openai-gateway"]
