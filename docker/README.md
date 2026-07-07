# sqry-mcp container image

Runs the [sqry](https://sqry.dev) MCP server (`sqry-mcp`, 38 tools, stdio) in a
container. The image installs the prebuilt release binary, so it is small and
builds in seconds rather than compiling the Rust and tree-sitter workspace.

The runtime is `distroless/cc`. The release binaries are glibc-dynamic (`ldd`:
glibc + `libstdc++6`), with no musl/static build in the release matrix, so
distroless/cc is the minimal base that carries exactly those libraries and
nothing else. A `FROM scratch` image would need a static (musl) build target
added upstream.

## Build

```bash
# Docker (amd64 or arm64)
docker build -t sqry-mcp -f docker/Dockerfile docker

# Podman (rootless-friendly)
podman build -t sqry-mcp -f docker/Containerfile docker

# Pin a version (defaults to the file's SQRY_VERSION)
docker build --build-arg SQRY_VERSION=27.0.8 -t sqry-mcp -f docker/Dockerfile docker

# Multi-arch
docker buildx build --platform linux/amd64,linux/arm64 -t sqry-mcp -f docker/Dockerfile docker
```

## Run

sqry indexes a repo on disk, so bind-mount your checkout at `/workspace`:

```bash
# Docker: run as your host uid so the mount (and the .sqry index) are writable
docker run -i --rm -v "$PWD:/workspace" --user "$(id -u):$(id -g)" sqry-mcp

# Podman: rootless maps container root to your user, so no --user is needed;
# add :Z on SELinux hosts
podman run -i --rm -v "$PWD:/workspace:Z" sqry-mcp
```

The image sets `SQRY_REDACTION_PRESET=relative`, so results come back as
workspace-relative paths (`src/foo.rs`) that map cleanly to the host checkout.

## As an MCP server

Point your agent at `docker run` as the MCP command (stdio):

```json
{
  "mcpServers": {
    "sqry": {
      "command": "docker",
      "args": ["run", "-i", "--rm",
               "-v", "${workspaceFolder}:/workspace",
               "--user", "1000:1000",
               "sqry-mcp"]
    }
  }
}
```

Replace `1000:1000` with your uid:gid, or use the Podman form. Set
`SQRY_CACHE_ROOT` to a named volume if you want the index cached outside the
mounted repo.

## Notes

- Bind-mount I/O is native on Linux; on macOS and Windows it runs through the
  Docker/Podman VM and is slower on large repos. For heavy local use the native
  binary (`cargo install sqry-mcp`, Homebrew, or the install script) is faster.
- The image carries `io.modelcontextprotocol.server.name=dev.sqry/sqry`, so a
  published image self-certifies to the official MCP Registry as an `oci`
  package.
