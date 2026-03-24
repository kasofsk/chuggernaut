# Multi-stage build with cargo-chef for cached dependency builds
FROM rust:1.88-bookworm AS chef
RUN cargo install cargo-chef
WORKDIR /build

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p chuggernaut-dispatcher
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p chuggernaut-dispatcher

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/chuggernaut-dispatcher /usr/local/bin/chuggernaut-dispatcher
