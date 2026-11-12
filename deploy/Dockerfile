# docker build -f deploy/Dockerfile -t jev-mcp:latest .
FROM rust:1.99-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7 AS rust
WORKDIR /build
RUN rustup target add x86_64-unknown-linux-musl \
    && apt-get update -qq \
    && apt-get install -y -qq musl-tools \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release --bins --target x86_64-unknown-linux-musl

# No libc, shell or CA store: TLS roots are bundled (webpki-roots), and ci.yml fails if
# a release dependency ever reaches for a system trust store.
FROM scratch
COPY --from=rust /build/target/x86_64-unknown-linux-musl/release/jev-mcp /usr/local/bin/jev-mcp
USER 65532:65532
EXPOSE 8000
ARG GIT_SHA=unknown
ENV JEV_MCP_GIT_SHA=$GIT_SHA
CMD ["jev-mcp"]
