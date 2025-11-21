# syntax=docker/dockerfile:1.7

FROM rust:1-bookworm AS builder

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates pkg-config build-essential \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install nightly && rustup default nightly

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates nodejs \
    && rm -rf /var/lib/apt/lists/*

RUN printf '{ "type": "module" }\n' > /usr/local/bin/package.json

RUN useradd -m app
WORKDIR /home/app

COPY --from=builder /app/target/release/claude-code-openai-gateway /usr/local/bin/claude-code-openai-gateway

USER app
EXPOSE 8080
ENV RUST_LOG=info

CMD ["claude-code-openai-gateway"]
