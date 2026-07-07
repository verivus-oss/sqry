# syntax=docker/dockerfile:1
#
# sqry-mcp: semantic code search MCP server (stdio transport).
#
# The published sqry release binaries are glibc-dynamic: `ldd` shows they need
# glibc and libstdc++6, and there is no musl/static build in the release matrix.
# So the runtime is distroless/cc, which bundles exactly glibc + libstdc++ +
# libgcc + ca-certificates and nothing else (no shell, no package manager).
# A truly static `FROM scratch` image would require a musl build target upstream.
#
# Multi-arch: linux/amd64 and linux/arm64.
#   docker build -t sqry-mcp -f docker/Dockerfile docker
#   docker run -i --rm -v "$PWD:/workspace" --user "$(id -u):$(id -g)" sqry-mcp

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

# distroless/cc: glibc + libstdc++ + libgcc + ca-certs only (~22 MB), runs as
# the built-in nonroot user (uid 65532).
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
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
ENTRYPOINT ["/usr/local/bin/sqry-mcp", "--no-daemon"]
