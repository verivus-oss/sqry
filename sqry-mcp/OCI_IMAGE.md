# sqry MCP OCI Image

**Status**: Planned publish path  
**Audience**: Users who want to run `sqry-mcp` through an OCI container runtime

## Purpose

The sqry OCI image packages the local stdio MCP server as a container image for registries that support MCP package discovery. The image is not a remote HTTP server; it runs the same local stdio server binary used by `sqry mcp setup`.

## Expected Contract

- Image contains `sqry-mcp` as the runtime entrypoint
- Transport remains `stdio`
- Workspace is mounted from the host into the container
- `SQRY_MCP_WORKSPACE_ROOT` points at the mounted workspace path inside the container
- Image metadata includes `io.modelcontextprotocol.server.name`

## Expected Usage

Example `docker run` shape:

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

Expected `server.json` OCI package metadata:
- `identifier`: `ghcr.io/verivus-oss/sqry-mcp:vX.Y.Z`
- `runtimeArguments`:
  - named `--mount` with required bind value `type=bind,src={source_path},dst=/workspace`
  - named `-e` with fixed value `SQRY_MCP_WORKSPACE_ROOT=/workspace`

## Notes

- This is a local packaging/distribution path, not a hosted service.
- The OCI path must remain aligned with the standalone binary path:
  - binary install: `sqry-mcp`
  - OCI install: image entrypoint launches `sqry-mcp`
- Canonical GHCR image name: `ghcr.io/verivus-oss/sqry-mcp`.
- Tag policy: publish version-only tags (`vX.Y.Z`) in this track; do not publish `latest`.
- Target platforms: publish a multi-arch manifest for `linux/amd64` and `linux/arm64`.
- The release workflow and `server.json` must point at the exact published image/tag.
