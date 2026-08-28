# syntax=docker/dockerfile:1
#
# Slim, multi-arch container image for the sqry MCP server.
#
# sqry-mcp ships as a fully static musl binary (the release pipeline gates every
# musl artifact through a `file: statically linked` + `ldd: not a dynamic
# executable` proof, issue #395), and the server makes no outbound network
# calls. The runtime base is a minimal alpine rather than `FROM scratch` for one
# reason: the `semantic_diff` tool shells out to the `git` binary
# (sqry-core/src/git/worktree.rs resolve_ref_to_commit / worktree add), so a
# git-less image would ship a broken tool. alpine + git keeps the image small
# (a few MB) while every tool works. No ca-certificates: git here only does
# local ref/worktree operations against the mounted repo, never network I/O.
#
# The image is built from the published release asset rather than from source,
# so `docker build` is fast and reproducible anywhere (this is also what the
# Glama build check runs). Pass the version to build:
#
#   docker buildx build \
#     --platform linux/amd64,linux/arm64 \
#     --build-arg SQRY_VERSION=30.0.0 \
#     -t ghcr.io/verivus-oss/sqry-mcp:v30.0.0 \
#     packaging/docker
#
# Run it against a host checkout (answers MCP initialize + tools/list):
#
#   docker run -i --rm -v "$PWD:/workspace" ghcr.io/verivus-oss/sqry-mcp
#
# ---------------------------------------------------------------------------
# Stage 1: fetch + verify the prebuilt static binary for the target arch.
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM alpine:3.21 AS fetch

# buildx sets TARGETARCH per requested platform (amd64 / arm64).
ARG TARGETARCH
ARG SQRY_VERSION

RUN apk add --no-cache curl coreutils

WORKDIR /out
RUN set -eux; \
    # Map the buildx arch token to the release asset arch token.
    case "${TARGETARCH}" in \
      amd64) asset="sqry-mcp-linux-x86_64-musl" ;; \
      arm64) asset="sqry-mcp-linux-arm64-musl" ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    # With no SQRY_VERSION the build tracks the newest published release, so a
    # bare `docker build` (what the Glama build check runs) works unchanged.
    if [ -n "${SQRY_VERSION:-}" ]; then \
      base="https://github.com/verivus-oss/sqry/releases/download/v${SQRY_VERSION}"; \
    else \
      base="https://github.com/verivus-oss/sqry/releases/latest/download"; \
    fi; \
    curl -fsSL -o sqry-mcp "${base}/${asset}"; \
    curl -fsSL -o SHA256SUMS.txt "${base}/SHA256SUMS.txt"; \
    # SHA256SUMS.txt lines are "<sha>  <asset-name>"; re-point the checked name
    # at the downloaded file, then verify. A checksum mismatch fails the build.
    grep "  ${asset}$" SHA256SUMS.txt | awk '{print $1"  sqry-mcp"}' > sqry-mcp.sha256; \
    test -s sqry-mcp.sha256 || { echo "no checksum found for ${asset}" >&2; exit 1; }; \
    sha256sum -c sqry-mcp.sha256; \
    chmod 0755 sqry-mcp

# ---------------------------------------------------------------------------
# Stage 2: the runtime image, alpine + git + the static binary.
# ---------------------------------------------------------------------------
FROM alpine:3.21

ARG SQRY_VERSION

# git is required by semantic_diff (ref resolution + worktree diffing). Local
# operations only, so no ca-certificates.
RUN apk add --no-cache git

COPY --from=fetch /out/sqry-mcp /usr/local/bin/sqry-mcp

LABEL org.opencontainers.image.title="sqry-mcp" \
      org.opencontainers.image.description="Local stdio MCP server for sqry semantic code search (39 tools, 37 languages)" \
      org.opencontainers.image.url="https://github.com/verivus-oss/sqry" \
      org.opencontainers.image.source="https://github.com/verivus-oss/sqry" \
      org.opencontainers.image.version="${SQRY_VERSION}" \
      org.opencontainers.image.licenses="MIT" \
      io.modelcontextprotocol.server.name="io.github.verivus-oss/sqry"

# The server indexes whatever is mounted at /workspace. SQRY_REDACTION_PRESET
# =relative keeps results workspace-relative so they map back to the host
# checkout. Relocate the index cache with SQRY_CACHE_ROOT if the mount is
# read-only.
WORKDIR /workspace
ENV SQRY_MCP_WORKSPACE_ROOT=/workspace \
    SQRY_REDACTION_PRESET=relative

ENTRYPOINT ["sqry-mcp", "--no-daemon"]
