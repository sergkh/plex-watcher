FROM rust:1.95 AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

FROM gcr.io/distroless/cc-debian13:nonroot

COPY --from=builder /app/target/release/plex-watcher /

EXPOSE 8000

ENTRYPOINT ["/plex-watcher"]
