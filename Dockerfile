FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p radar-server
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* && useradd --system --uid 10001 --home /nonexistent radial
COPY --from=build /src/target/release/radar-server /usr/local/bin/radar-server
RUN mkdir /data && chown radial:radial /data
USER 10001
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s CMD curl -fsS http://127.0.0.1:8080/health || exit 1
ENTRYPOINT ["radar-server"]

