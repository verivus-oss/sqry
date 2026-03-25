# sqry MCP OCI Image

**Status**: Repo-ready container contract; active public release publication pending  
**Audience**: Users who want to understand or locally validate the `sqry-mcp` OCI packaging path

## Purpose

The sqry OCI image packages the local stdio MCP server as a container image. The image is not a remote HTTP server; it runs the same local `sqry-mcp` binary used by standalone MCP setups.

## Current State

- `packaging/docker/Dockerfile` builds `sqry-mcp`
- image entrypoint is `sqry-mcp`
- working directory is `/workspace`
- `SQRY_MCP_WORKSPACE_ROOT=/workspace` is set in the image
- image metadata includes `io.modelcontextprotocol.server.name`
- `.mcp/server.json` describes the OCI package contract
- active public release workflow does **not** yet publish the GHCR image
- OSS sanitization does **not** yet ship `.mcp/server.json` to the public repo

## Runtime Contract

Expected `docker run` shape:

```bash
docker run --rm -i \
  --mount type=bind,src="$PWD",dst=/workspace \
  -e SQRY_MCP_WORKSPACE_ROOT=/workspace \
  ghcr.io/verivus-oss/sqry-mcp:vX.Y.Z
```

Podman uses the same contract:

```bash
podman run --rm -i \
  --mount type=bind,src="$PWD",dst=/workspace \
  -e SQRY_MCP_WORKSPACE_ROOT=/workspace \
  ghcr.io/verivus-oss/sqry-mcp:vX.Y.Z
```

## `server.json` Contract

`.mcp/server.json` currently records:
- `identifier`: `ghcr.io/verivus-oss/sqry-mcp:vX.Y.Z`
- `transport.type`: `stdio`
- `runtimeArguments`:
  - named `--mount` with value `type=bind,src={source_path},dst=/workspace`
  - named `-e` with value `SQRY_MCP_WORKSPACE_ROOT=/workspace`

## Important Limitation

The repository contains the OCI contract, but the active public release workflow still does not publish/sign `ghcr.io/verivus-oss/sqry-mcp:vX.Y.Z`. Until that lands in `.github/workflows/oss-leg3-release.yml`, treat this page as:
- source-of-truth for the intended runtime contract
- valid for local Docker/Podman builds from this repository
- not yet proof of a supported public pull path from GHCR

## Notes

- This is a local packaging/distribution path, not a hosted service.
- Canonical image name remains `ghcr.io/verivus-oss/sqry-mcp`.
- Tag policy remains version-only (`vX.Y.Z`), not `latest`.
- Target platforms remain `linux/amd64` and `linux/arm64` once active release publication is wired.
