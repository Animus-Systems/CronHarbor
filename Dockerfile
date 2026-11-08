# syntax=docker/dockerfile:1
FROM rust:1.99.1-slim-bookworm@sha256:f2c004e3d2d8b547e8cf7ffbc29205385f4e71d23e53e4e383065db5e9f822cd AS source
ARG TARGETARCH
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY migrations ./migrations
COPY tests ./tests
COPY examples ./examples
COPY schema ./schema

FROM source AS checks
RUN apt-get update && apt-get install --yes --no-install-recommends python3 openssl \
    && rm -rf /var/lib/apt/lists/*
RUN --mount=type=cache,id=cronharbor-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cronharbor-target-${TARGETARCH},target=/build/target \
    cargo test --locked --all-features --all-targets && cargo clippy --locked --all-features --all-targets -- -D warnings

FROM source AS core-build
RUN --mount=type=cache,id=cronharbor-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cronharbor-target-${TARGETARCH},target=/build/target \
    cargo build --locked --release && mkdir /out && cp target/release/cronharbor /out/cronharbor
FROM source AS api-build
RUN --mount=type=cache,id=cronharbor-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cronharbor-target-${TARGETARCH},target=/build/target \
    cargo build --locked --release --features api && mkdir /out && cp target/release/cronharbor /out/cronharbor
FROM source AS full-build
RUN --mount=type=cache,id=cronharbor-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cronharbor-target-${TARGETARCH},target=/build/target \
    cargo build --locked --release --features full && mkdir /out && cp target/release/cronharbor /out/cronharbor

FROM node:22-bookworm-slim@sha256:5ec23b7c622b00547753bc6ffd78085dd868a68aeb41c8e0a1418f033e13d96d AS dashboard
WORKDIR /web
COPY web/dashboard/package.json web/dashboard/package-lock.json ./
RUN npm ci
COPY web/dashboard ./
RUN npm test && npm run build

FROM docker:cli@sha256:2357639ad1ea7d0194134bf3292622fd4e562483966ed674e5d7c945b5f3b28d AS docker-client
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc AS runtime
RUN apt-get update && apt-get install --yes --no-install-recommends util-linux ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 cronharbor \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin cronharbor \
    && install -d -m 0700 -o 10001 -g 10001 /var/lib/cronharbor
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/cronharbor", "--state-dir", "/var/lib/cronharbor"]
CMD ["server"]
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s CMD ["/usr/local/bin/cronharbor", "--state-dir", "/var/lib/cronharbor", "status"]

FROM runtime AS api
COPY --from=api-build /out/cronharbor /usr/local/bin/cronharbor
CMD ["server", "--listen", "0.0.0.0:8090"]

FROM runtime AS full
COPY --from=full-build /out/cronharbor /usr/local/bin/cronharbor
COPY --from=dashboard /web/dist /usr/share/cronharbor/ui
COPY --from=docker-client /usr/local/bin/docker /usr/bin/docker
CMD ["server", "--listen", "0.0.0.0:8090", "--ui-dir", "/usr/share/cronharbor/ui"]

FROM runtime AS core
COPY --from=core-build /out/cronharbor /usr/local/bin/cronharbor
