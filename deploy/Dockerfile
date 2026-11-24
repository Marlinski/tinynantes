# Tiny Nantes: the Rust server (live feeds, players, chat, places, sims) serving the viewer on :8080.
# Build context = the tarball made by deploy/package.sh (server/ sources, web/, the crawled places).
#
# The painted map is NOT in the image by default: it lives on its own volume mounted at /tiles (TN_TILES), filled
# by sysiphe from minix:~/Work/tinynantes-tiles/ (deploy/sync-tiles.sh mirrors it there, incrementally). With a
# "full" context (deploy/package.sh full) the map is inside web/data and the volume can be left out: the server
# falls back to web/data when /tiles has no headings.json.
#
# Runtime state lives in /data (persistent volume): SQLite database (accounts, chat and talk logs, feed snapshots,
# flight routes) and downloaded timetables (Naolib + SNCF GTFS, refreshed daily). The pod needs outbound HTTPS.

FROM rust:1-bookworm AS build
WORKDIR /src
# dependencies first, in their own layer: a code change rebuilds only our crate (~1 min instead of 5-10)
COPY server/Cargo.toml server/Cargo.lock server/rust-toolchain.toml ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release --locked && rm -rf src
COPY server/src ./src
RUN touch src/main.rs && cargo build --release --locked && strip target/release/tinynantes-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget && rm -rf /var/lib/apt/lists/* \
 && useradd -u 10001 -m tn && mkdir -p /data /tiles && chown tn /data
COPY --from=build /src/target/release/tinynantes-server /usr/local/bin/
COPY places/ /app/places/
COPY web/ /app/web/
ENV TN_WEB=/app/web TN_DATA=/data TN_TILES=/tiles TN_PLACES=/app/places TN_PORT=8080 RUST_LOG=info
USER tn
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1:8080/healthz || exit 1
CMD ["tinynantes-server"]
