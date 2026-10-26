# Container image for the tkmd stdio MCP server.
# Glama (https://glama.ai/mcp/servers) builds this to start the server and
# run MCP introspection. Mount a project at /workspace to manage its cards.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin tkmd

FROM debian:bookworm-slim
# git is needed for the eval-check pin (see issue #4).
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/tkmd /usr/local/bin/tkmd
WORKDIR /workspace
ENTRYPOINT ["tkmd", "mcp"]
