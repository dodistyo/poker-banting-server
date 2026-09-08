# Poker-banting server — multi-stage build, final stage = scratch.
#
# Stage 1 (builder): rust:1.88-alpine. Alpine's libc is musl, and on a musl
#   target Rust links FULLY STATIC by default (no glibc, no dynamic loader).
#   The dependency tree is pure Rust (no C / openssl / cc), so a plain
#   `cargo build --release` yields a self-contained static binary — exactly
#   what `FROM scratch` needs. (We can't use the official -musl tag here:
#   `rustup target add x86_64-unknown-linux-musl` downloads from
#   static.rust-lang.org, which is unreachable in the build env, so we build
#   natively on an image that already IS musl.)
#
# Stage 2 (final): scratch. No OS, no shell, no glibc — just the binary and
#   the CA bundle (needed for any outbound TLS, e.g. Google's health checks).
FROM rust:1.88-alpine AS builder
WORKDIR /app

# Copy the whole source up front. The app is small, so the dependency cache
# isn't the bottleneck — a single fetch+build layer is simpler and avoids the
# "no targets specified" error that `cargo fetch` hits before src/ exists.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo fetch \
    && cargo build --release

# ---- final: scratch -------------------------------------------------------
FROM scratch
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /app/target/release/poker-banting-server /poker-banting-server

# Cloud Run injects $PORT for the ingress container.
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/poker-banting-server"]
