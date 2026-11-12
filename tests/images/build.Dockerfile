# Builds and checks the host: Rust with clippy and rustfmt, and what the build and the tests call.
FROM rust:1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends nasm ffmpeg python3 && rm -rf /var/lib/apt/lists/* \
    && rustup component add clippy rustfmt
