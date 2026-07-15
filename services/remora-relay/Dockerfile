FROM rust:1.97.0-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 remora \
    && install -d -o remora -g remora /data
COPY --from=build /build/target/release/remora-relay /usr/local/bin/remora-relay
USER remora
EXPOSE 8787
ENTRYPOINT ["remora-relay"]
CMD ["serve", "--config", "/etc/remora-relay/config.toml"]
