# syntax=docker/dockerfile:1
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked && mkdir /data

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/mbwatch /usr/local/bin/mbwatch
COPY --from=build --chown=65532:65532 /data /data
ENV MBWATCH_CACHE_DIR=/data
VOLUME /data
ENTRYPOINT ["/usr/local/bin/mbwatch"]
CMD ["--help"]
