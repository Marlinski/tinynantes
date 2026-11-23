# Tiny Nantes: the Rust server (live feeds, players, chat, places) serving the viewer and the tile pyramids on :8080.
# Build context = the tarball made by deploy/package.sh (server/ sources, web/ including web/data/ which is not on
# GitHub, and the crawled places).
#
# Runtime state lives in /data (mount a persistent volume): SQLite database (accounts, chat log, feed snapshots,
# flight routes) and downloaded timetables (Naolib + SNCF GTFS, refreshed daily). The pod needs outbound HTTPS.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY server/ ./
RUN cargo build --release --locked && strip target/release/tinynantes-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget && rm -rf /var/lib/apt/lists/* \
 && useradd -u 10001 -m tn && mkdir -p /data && chown tn /data
COPY --from=build /src/target/release/tinynantes-server /usr/local/bin/
COPY web/ /app/web/
COPY places/ /app/places/
ENV TN_WEB=/app/web TN_DATA=/data TN_PLACES=/app/places TN_PORT=8080 RUST_LOG=info
USER tn
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1:8080/healthz || exit 1
CMD ["tinynantes-server"]
