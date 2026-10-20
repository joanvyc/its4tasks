# its4tasks-server with the web GUI built in, for Railway or any container
# host. README.md lists the settings; the database backend is chosen at
# build time:
#   docker build -t its4tasks .                          # Postgres
#   docker build --build-arg FEATURES=sqlite -t its4tasks .

FROM rust:1.99 AS build
ARG TRUNK_VERSION=0.21.14
ARG FEATURES=postgres
RUN rustup target add wasm32-unknown-unknown \
 && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-x86_64-unknown-linux-gnu.tar.gz" \
    | tar -xz -C /usr/local/bin trunk
WORKDIR /src
COPY . .
# The web GUI first: the release server embeds crates/gui/dist.
RUN cd crates/gui && trunk build --release
RUN cargo build --release --locked -p its4tasks-server --no-default-features --features "$FEATURES"

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 its4tasks
COPY --from=build /src/target/release/its4tasks-server /usr/local/bin/its4tasks-server
USER its4tasks
EXPOSE 8080
# Listens on $ITS4TASKS_ADDR if set, else on all interfaces at $PORT, which
# Railway sets, else 8080.
CMD ["sh", "-c", "ITS4TASKS_ADDR=\"${ITS4TASKS_ADDR:-0.0.0.0:${PORT:-8080}}\" exec its4tasks-server"]
