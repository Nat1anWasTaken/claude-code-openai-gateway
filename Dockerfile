# syntax=docker/dockerfile:1.7

### Build stage ###
FROM rust:1.82-slim AS builder

WORKDIR /app

# Install minimal build deps then clean apt caches.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates pkg-config build-essential \
    && rm -rf /var/lib/apt/lists/*

# Leverage layer caching for dependencies.
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release

### Runtime stage ###
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

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
