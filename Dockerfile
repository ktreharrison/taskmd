# Container image for the tkmd stdio MCP server.
# Glama (https://glama.ai/mcp/servers) builds this to start the server and
# run MCP introspection. Mount a project at /workspace to manage its cards:
#   docker run -i --rm --user "$(id -u):$(id -g)" -v "$PWD:/workspace" tkmd
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
# --local stores cards in /workspace/tasks even without a .tkmd.toml marker;
# without it tkmd falls back to a global vault inside the container, which is
# lost when the container exits.
# HOME=/tmp keeps tkmd's config writable under any --user UID.
ENV HOME=/tmp
ENTRYPOINT ["tkmd", "mcp", "--local"]
