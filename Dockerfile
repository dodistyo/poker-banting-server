# Poker-banting server — multi-stage build, final stage = scratch.
#
# Stage 1 (builder): rust:1.88-bookworm (glibc host). We CROSS-compile for
#   x86_64-unknown-linux-musl. Why not build natively on rust:alpine?
#   Proc-macros (axum/tokio use several) compile to HOST dylibs, and a musl
#   host defaults to a static CRT, which can't link host dylibs
#   ("cannot find crti.o" / cargo#7563). A glibc host cross-compiling to musl
#   links proc-macros against the host's glibc (they never ship — only the
#   final binary is copied out), while the FINAL binary is musl → fully
#   static, no dynamic loader → runs on `FROM scratch`.
FROM rust:1.88-bookworm AS builder
# musl-tools provides the musl C compiler that `ring`/`rustls` (pulled in
# by the redis `tokio-rustls-comp` TLS feature) need when cross-compiling to
# musl — `cc` looks for `x86_64-linux-musl-gcc`. On Debian bookworm
# musl-tools already ships that name as a symlink to musl-gcc; the ln below
# is a no-op guard for distros where only `musl-gcc` exists (must be
# tolerant — `ln -sf` on an existing symlink still exits 1).
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && (ln -sf /usr/bin/musl-gcc /usr/bin/x86_64-linux-musl-gcc 2>/dev/null || true) \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl
WORKDIR /app

# Copy the whole source up front. The app is small, so a single
# fetch+build layer is simpler and avoids the "no targets specified" error
# that `cargo fetch` hits before src/ exists.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo fetch --target x86_64-unknown-linux-musl \
    && cargo build --release --target x86_64-unknown-linux-musl

# ---- final: scratch -------------------------------------------------------
FROM scratch
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/poker-banting-server /poker-banting-server

# Cloud Run injects $PORT for the ingress container (the app resolves
# SERVER_PORT > PORT > 8080 in config).
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/poker-banting-server"]
