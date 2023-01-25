# syntax=docker/dockerfile:1.4

FROM rust:1.66.1-slim-bullseye as builder

WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/usr/local/rustup \
    set -eux; \
    rustup install stable; \
    cargo build --release; \
    objcopy --compress-debug-sections /app/target/release/site-shim ./site-shim

FROM debian:bullseye-slim

WORKDIR /app
COPY --from=builder /app/site-shim ./site-shim
ENTRYPOINT "./site-shim"
