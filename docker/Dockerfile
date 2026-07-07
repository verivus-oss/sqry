# syntax=docker/dockerfile:1
#
# sqry-mcp: semantic code search MCP server (stdio transport).
#
# Installs the prebuilt release binary instead of compiling the Rust +
# tree-sitter workspace, so the image is small and builds in seconds.
# Multi-arch: linux/amd64 and linux/arm64.
#
#   docker build -t sqry-mcp -f docker/Dockerfile docker
#   docker run -i --rm -v "$PWD:/workspace" --user "$(id -u):$(id -g)" sqry-mcp
#
# See docker/README.md for MCP client config and the bind-mount notes.

ARG SQRY_VERSION=27.0.8

FROM debian:stable-slim AS fetch
ARG SQRY_VERSION
ARG TARGETARCH
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl; \
    case "${TARGETARCH:-amd64}" in \
      amd64) arch="x86_64" ;; \
      arm64) arch="arm64" ;; \
      *) echo "unsupported TARGETARCH=${TARGETARCH:-}" >&2; exit 1 ;; \
    esac; \
    curl -fsSL --retry 3 --proto '=https' --tlsv1.2 \
      -o /sqry-mcp \
      "https://github.com/verivus-oss/sqry/releases/download/v${SQRY_VERSION}/sqry-mcp-linux-${arch}"; \
    chmod 0755 /sqry-mcp; \
    /sqry-mcp --version

FROM debian:stable-slim AS runtime
# Runtime deps for the glibc-linked binary: libstdc++6 (libgcc-s1, libc6 are in base).
RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates libstdc++6; \
    rm -rf /var/lib/apt/lists/*; \
    useradd --create-home --uid 10001 --shell /usr/sbin/nologin sqry; \
    mkdir -p /workspace; chown sqry:sqry /workspace

COPY --from=fetch /sqry-mcp /usr/local/bin/sqry-mcp

LABEL org.opencontainers.image.title="sqry-mcp" \
      org.opencontainers.image.description="Semantic code search MCP server (stdio)." \
      org.opencontainers.image.source="https://github.com/verivus-oss/sqry" \
      org.opencontainers.image.url="https://sqry.dev" \
      org.opencontainers.image.licenses="MIT" \
      io.modelcontextprotocol.server.name="dev.sqry/sqry"

# Bind-mount the repo at /workspace. Standalone (no daemon probe); relative-path
# output so results map cleanly back to the host checkout.
ENV SQRY_MCP_WORKSPACE_ROOT=/workspace \
    SQRY_REDACTION_PRESET=relative
WORKDIR /workspace
USER sqry
ENTRYPOINT ["sqry-mcp", "--no-daemon"]
