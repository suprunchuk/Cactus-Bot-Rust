# syntax=docker/dockerfile:1
FROM rust:1-slim-bookworm AS chef
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --locked
WORKDIR /build

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY --from=planner /build/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked && cp /build/target/release/cactus_bot /cactus_bot

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && groupadd --system app && useradd --system --gid app --home-dir /nonexistent --shell /usr/sbin/nologin app \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build --chown=app:app /cactus_bot /usr/local/bin/cactus_bot
USER app:app
ENTRYPOINT ["cactus_bot"]
