FROM docker.io/library/rust:1.74-alpine3.18 as builder

RUN apk add musl-dev

WORKDIR /usr/src/chetter-app

# Build (and cache) only dependencies which are less likely to change
RUN cargo init
COPY Cargo.toml Cargo.lock ./
RUN cargo build --release
RUN cargo clean -p chetter-app

# Build actual app
COPY ./src src
RUN cargo install --path . --locked --offline

# Prior image has all build deps, easier to start fresh to clean
FROM alpine:3.18
COPY --from=builder /usr/local/cargo/bin/* /usr/local/bin

ENTRYPOINT ["/usr/local/bin/chetter-app", "--config", "/config/chetter-app.toml"]

VOLUME /config
EXPOSE 3333/tcp
