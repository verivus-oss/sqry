import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as https from "node:https";
import type { ClientRequest, IncomingMessage } from "node:http";
import * as path from "node:path";
import * as url from "node:url";
import * as vscode from "vscode";
import { unzipSync } from "fflate";

const GITHUB_RELEASE_BASE = "https://github.com/verivus-oss/sqry/releases/download";
const OIDC_ISSUER = "https://token.actions.githubusercontent.com";
const CERT_IDENTITY_WORKFLOWS = ["release-distribute.yml", "oss-distribute.yml"] as const;
const MAX_DOWNLOAD_SIZE = 200 * 1024 * 1024; // 200 MB
const LOCK_STALE_MS = 10 * 60 * 1000; // 10 minutes
const KEEP_VERSIONS = 2;
const SIGSTORE_VERIFY_TIMEOUT_MS = 30_000;
const SIGSTORE_TUF_CACHE_DIR = "sigstore-tuf-cache";
const CHECKSUM_ASSET_NAMES = ["SHA256SUMS.txt", "CHECKSUMS.sha256"] as const;
const ATTESTATION_BUNDLE_NAMES = ["release-artifacts.attestation.json"] as const;
const DEFAULT_DOWNLOAD_TIMEOUTS = {
  responseTimeoutMs: 30_000,
  idleTimeoutMs: 30_000,
} as const;

class ReleaseAssetUnavailableError extends Error {}

/** Bounded transport waits used by binary-download lifecycle tests and production. */
export interface DownloadTimeouts {
  /** Maximum time for the complete redirect chain to produce final response headers. */
  readonly responseTimeoutMs: number;
  /** Maximum quiet interval between final-response body chunks. */
  readonly idleTimeoutMs: number;
}

/** Raised when the caller cancels binary download before it can complete. */
export class DownloadCancelledError extends Error {
  constructor() {
    super("Download cancelled");
    this.name = "DownloadCancelledError";
  }
}

/** Raised when a download cannot make required transport progress. */
export class DownloadTimeoutError extends Error {
  constructor(stage: "response" | "idle") {
    super(
      stage === "response"
        ? "Download timed out waiting for response headers"
        : "Download timed out waiting for body progress",
    );
    this.name = "DownloadTimeoutError";
  }
}

export interface PlatformInfo {
  /**
   * Release asset to download. May contain the literal `{version}` token, which
   * the download orchestrator substitutes with the effective version (the
   * Windows zip is named `sqry-<version>-windows-x86_64.zip`).
   */
  asset: string;
  /** The binary that actually gets launched (`sqry` / `sqry.exe`). */
  binaryName: string;
  /**
   * When set, `asset` is an archive to download+verify+extract rather than a
   * bare binary. `memberBinary` is the launch binary inside the archive; all
   * `.dll` members are extracted alongside it so the binary is self-contained.
   */
  archive?: {
    format: "zip";
    memberBinary: string;
  };
}

export interface VersionInfo {
  version: string;
  downloadedAt: string;
  platform: string;
  sha256: string;
  requestedVersion?: string;
}

export function getChecksumAssetCandidates(): readonly string[] {
  return CHECKSUM_ASSET_NAMES;
}

export function getBundleAssetCandidates(assetName: string): string[] {
  return [...ATTESTATION_BUNDLE_NAMES, `${assetName}.bundle`];
}

function isDevelopmentLikeMode(extensionMode: vscode.ExtensionMode): boolean {
  return extensionMode === vscode.ExtensionMode.Development || extensionMode === vscode.ExtensionMode.Test;
}

export function getDownloadVersionCandidates(
  version: string,
  extensionMode: vscode.ExtensionMode,
): readonly string[] {
  if (!isDevelopmentLikeMode(extensionMode)) {
    return [version];
  }

  const versionMatch = /^(\d+)\.(\d+)\.(\d+)$/.exec(version);
  if (!versionMatch) {
    return [version];
  }

  const [, major, minor, patch] = versionMatch;
  const maxPatch = Number.parseInt(patch, 10);
  const candidates: string[] = [];
  for (let currentPatch = maxPatch; currentPatch >= 0; currentPatch -= 1) {
    candidates.push(`${major}.${minor}.${currentPatch}`);
  }
  return candidates;
}

function getConfiguredProxyUrl(): string | undefined {
  const proxyUrl = vscode.workspace.getConfiguration("http").get<string>("proxy");
  const trimmedProxyUrl = proxyUrl?.trim();
  return trimmedProxyUrl ? trimmedProxyUrl : undefined;
}

export function buildSigstoreVerifyOptions(storageUri: vscode.Uri): {
  timeout: number;
  tufCachePath: string;
} {
  return {
    timeout: SIGSTORE_VERIFY_TIMEOUT_MS,
    tufCachePath: path.join(storageUri.fsPath, SIGSTORE_TUF_CACHE_DIR),
  };
}

export async function withTemporarySigstoreProxyEnv<T>(
  proxyUrl: string | undefined,
  operation: () => Promise<T>,
): Promise<T> {
  if (!proxyUrl) {
    return operation();
  }

  const proxyEnvKeys = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] as const;
  const updatedKeys: Array<(typeof proxyEnvKeys)[number]> = [];

  for (const proxyEnvKey of proxyEnvKeys) {
    if (!process.env[proxyEnvKey]) {
      process.env[proxyEnvKey] = proxyUrl;
      updatedKeys.push(proxyEnvKey);
    }
  }

  try {
    return await operation();
  } finally {
    for (const proxyEnvKey of updatedKeys) {
      delete process.env[proxyEnvKey];
    }
  }
}

/**
 * Detect the platform and return the corresponding release asset name.
 * Throws for unsupported platforms with descriptive messages.
 */
export function detectPlatform(): PlatformInfo {
  const platform = process.platform;
  const arch = process.arch;

  // Linux: use the fully static musl builds so the binary runs on any distro
  // (any glibc version, and musl distros like Alpine) without a preflight
  // failure from a missing/incompatible libc.
  if (platform === "linux" && arch === "x64") {
    return { asset: "sqry-linux-x86_64-musl", binaryName: "sqry" };
  }
  if (platform === "linux" && arch === "arm64") {
    return { asset: "sqry-linux-arm64-musl", binaryName: "sqry" };
  }
  // Windows: the bare exe depends on MinGW runtime DLLs (libstdc++-6.dll,
  // libgcc_s_seh-1.dll, libwinpthread-1.dll) that a clean machine does not
  // have. The release zip bundles those DLLs, so download and extract it.
  if (platform === "win32" && arch === "x64") {
    return {
      asset: "sqry-{version}-windows-x86_64.zip",
      binaryName: "sqry.exe",
      archive: { format: "zip", memberBinary: "sqry.exe" },
    };
  }
  if (platform === "darwin" && arch === "arm64") {
    return { asset: "sqry-macos-arm64", binaryName: "sqry" };
  }
  if (platform === "darwin" && arch === "x64") {
    return { asset: "sqry-macos-x86_64", binaryName: "sqry" };
  }

  throw new Error(
    `sqry does not provide pre-built binaries for ${platform}-${arch}. ` +
    "Install via: cargo install sqry-cli"
  );
}

/**
 * Read the binaryVersion field from the extension's package.json.
 */
export function getBinaryVersion(): string {
  const extPath = path.resolve(__dirname, "..");
  const pkgPath = path.join(extPath, "package.json");
  const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf-8"));
  const version = pkg.binaryVersion;
  if (!version || typeof version !== "string") {
    throw new Error("binaryVersion not found in package.json");
  }
  return version;
}

/**
 * Check if a host is in the allowed download host list.
 */
export function isAllowedHost(hostname: string): boolean {
  if (hostname === "github.com") {
    return true;
  }
  // Match *.githubusercontent.com but NOT github.com.evil.com
  if (hostname.endsWith(".githubusercontent.com")) {
    // Ensure there's exactly one label before .githubusercontent.com
    const prefix = hostname.slice(0, -".githubusercontent.com".length);
    return prefix.length > 0 && !prefix.includes(".");
  }
  return false;
}

/**
 * Download a file over HTTPS with progress reporting, redirect following,
 * proxy support, and security checks.
 */
export async function downloadWithProgress(
  downloadUrl: string,
  destPath: string,
  onProgress?: (downloaded: number, total: number) => void,
  cancellationToken?: vscode.CancellationToken,
  maxRedirects = 5,
  timeouts: DownloadTimeouts = DEFAULT_DOWNLOAD_TIMEOUTS,
): Promise<void> {
  return new Promise((resolve, reject) => {
    let settled = false;
    let completedSuccessfully = false;
    // Redirects create more than one live request/response pair. Keep every
    // hop owned until its stream closes so a terminal timeout or cancellation
    // cannot leave a drained parent request running after its child starts.
    const activeRequests = new Set<ClientRequest>();
    const activeResponses = new Set<IncomingMessage>();
    const redirectRequests = new Set<ClientRequest>();
    const redirectResponses = new Set<IncomingMessage>();
    let fileStream: fs.WriteStream | undefined;
    let cancelDisposable: vscode.Disposable | undefined;
    let responseTimer: NodeJS.Timeout | undefined;
    let idleTimer: NodeJS.Timeout | undefined;
    let failureFinished = false;

    const clearTimers = (): void => {
      if (responseTimer) {
        clearTimeout(responseTimer);
        responseTimer = undefined;
      }
      if (idleTimer) {
        clearTimeout(idleTimer);
        idleTimer = undefined;
      }
    };

    const disposeCancellation = (): void => {
      cancelDisposable?.dispose();
      cancelDisposable = undefined;
    };

    const finishFailure = (error: Error): void => {
      if (failureFinished) {
        return;
      }
      failureFinished = true;
      if (fileStream) {
        cleanupFile(destPath);
      }
      reject(error);
    };

    const failOnce = (error: Error): void => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimers();
      disposeCancellation();

      const stream = fileStream;
      if (stream && !stream.closed) {
        // Windows can reject unlink while the write handle remains open. The
        // public promise must not reject (and let outer cleanup race) until
        // close has released the handle and the partial file is removed.
        stream.once("close", () => finishFailure(error));
      }

      // The local error is already authoritative. Destroy without passing it
      // back into Node streams so a synchronous redirect/HTTP-error path
      // cannot emit an unhandled secondary `error` event before listeners are
      // attached.
      for (const request of activeRequests) {
        request.destroy();
      }
      for (const response of activeResponses) {
        response.destroy();
      }
      stream?.destroy();

      if (!stream || stream.closed) {
        finishFailure(error);
      }
    };

    const closeIncompleteRedirectHops = (): void => {
      // `response.resume()` only drains a redirect parent. A misbehaving
      // transport can leave that parent open forever even after the redirected
      // child has completed successfully. The terminal asset is already fully
      // written when this runs, so close only still-live redirect parents and
      // leave the completed final response alone.
      for (const request of [...redirectRequests]) {
        request.destroy();
      }
      for (const response of [...redirectResponses]) {
        response.destroy();
      }
    };

    const succeedOnce = (): void => {
      if (settled) {
        return;
      }
      settled = true;
      completedSuccessfully = true;
      clearTimers();
      disposeCancellation();
      closeIncompleteRedirectHops();
      resolve();
    };

    const resetIdleTimer = (): void => {
      if (idleTimer) {
        clearTimeout(idleTimer);
      }
      idleTimer = setTimeout(() => {
        failOnce(new DownloadTimeoutError("idle"));
      }, timeouts.idleTimeoutMs);
    };

    const buildRequestOptions = (parsed: url.URL): https.RequestOptions => {
      const requestOptions: https.RequestOptions = {
        hostname: parsed.hostname,
        path: parsed.pathname + parsed.search,
        headers: { "User-Agent": "sqry-vscode" },
      };
      const proxyUrl = getConfiguredProxyUrl();
      if (proxyUrl) {
        try {
          // Dynamic require to allow the module to load even without the dep.
          const proxyAgentModule = require("https-proxy-agent");
          requestOptions.agent = new proxyAgentModule.HttpsProxyAgent(proxyUrl);
        } catch {
          // Fall through without a proxy if agent creation fails.
        }
      }
      return requestOptions;
    };

    const startHop = (currentUrl: string, redirectsRemaining: number): void => {
      if (settled) {
        return;
      }
      let parsed: url.URL;
      try {
        parsed = new url.URL(currentUrl);
      } catch {
        failOnce(new Error(`Invalid download URL: ${currentUrl}`));
        return;
      }

      if (parsed.protocol !== "https:") {
        failOnce(new Error(`Refusing non-HTTPS download URL: ${currentUrl}`));
        return;
      }
      if (!isAllowedHost(parsed.hostname)) {
        failOnce(new Error(`Download host not in allowlist: ${parsed.hostname}`));
        return;
      }

      let request: ClientRequest | undefined;
      let isRedirectHop = false;
      try {
        request = https.get(buildRequestOptions(parsed), (response) => {
          // A redirect parent is still a live Node stream after resume().
          // Attach lifecycle listeners before any branch drains it, so a late
          // error/abort is owned and terminal cleanup reaches every hop.
          activeResponses.add(response);
          response.once("close", () => {
            activeResponses.delete(response);
            redirectResponses.delete(response);
          });
          response.on("error", (error) => {
            failOnce(error);
          });
          response.on("aborted", () => {
            failOnce(new Error("Network response aborted before download completed"));
          });

          if (settled) {
            response.destroy();
            return;
          }

          if (
            response.statusCode &&
            response.statusCode >= 300 &&
            response.statusCode < 400 &&
            response.headers.location
          ) {
            isRedirectHop = true;
            redirectResponses.add(response);
            if (request) {
              redirectRequests.add(request);
            }
            response.resume();
            if (redirectsRemaining <= 0) {
              failOnce(new Error("Too many redirects"));
              return;
            }

            let redirectUrl: string;
            try {
              redirectUrl = new url.URL(response.headers.location, currentUrl).toString();
            } catch {
              failOnce(new Error(`Invalid redirect URL: ${response.headers.location}`));
              return;
            }
            startHop(redirectUrl, redirectsRemaining - 1);
            return;
          }

          if (response.statusCode === 404) {
            response.resume();
            failOnce(new ReleaseAssetUnavailableError("HTTP 404: Release asset not found"));
            return;
          }
          if (!response.statusCode || response.statusCode >= 400) {
            response.resume();
            failOnce(new Error(`HTTP error: ${response.statusCode}`));
            return;
          }

          const contentLength = parseContentLengthHeader(response.headers["content-length"]);
          if (contentLength > MAX_DOWNLOAD_SIZE) {
            response.resume();
            failOnce(
              new Error(
                `Download rejected: file exceeds expected size limit (${contentLength} bytes > ${MAX_DOWNLOAD_SIZE} bytes)`,
              ),
            );
            return;
          }

          if (responseTimer) {
            clearTimeout(responseTimer);
            responseTimer = undefined;
          }
          try {
            fileStream = fs.createWriteStream(destPath);
          } catch (error) {
            failOnce(error instanceof Error ? error : new Error(String(error)));
            return;
          }

          let downloaded = 0;
          resetIdleTimer();
          response.on("data", (chunk: Buffer) => {
            if (settled) {
              return;
            }
            resetIdleTimer();
            downloaded += chunk.length;
            if (downloaded > MAX_DOWNLOAD_SIZE) {
              failOnce(new Error("Download rejected: file exceeds expected size limit"));
              return;
            }
            if (onProgress && contentLength > 0) {
              try {
                onProgress(downloaded, contentLength);
              } catch (error) {
                failOnce(error instanceof Error ? error : new Error(String(error)));
              }
            }
          });
          fileStream.on("error", (error) => failOnce(error));
          fileStream.on("finish", succeedOnce);
          response.pipe(fileStream);
        });
      } catch (error) {
        failOnce(error instanceof Error ? error : new Error(String(error)));
        return;
      }

      if (!request) {
        failOnce(new Error("HTTPS request was not created"));
        return;
      }

      activeRequests.add(request);
      if (isRedirectHop) {
        redirectRequests.add(request);
      }
      request.once("close", () => {
        activeRequests.delete(request);
        redirectRequests.delete(request);
      });
      request.on("error", (error) => {
        failOnce(
          new Error(
            `Network error: ${error.message}. Check your internet connection and proxy settings.`,
          ),
        );
      });
      if (settled && (!completedSuccessfully || isRedirectHop)) {
        // A synchronous test transport can invoke its callback and finish the
        // final body before `https.get()` returns its request. A successful
        // final request is already complete in that case; only a failed hop
        // or a redirect parent still needs explicit destruction here.
        request.destroy();
      }
    };

    if (cancellationToken?.isCancellationRequested) {
      failOnce(new DownloadCancelledError());
      return;
    }
    const registeredCancellation = cancellationToken?.onCancellationRequested(() => {
      failOnce(new DownloadCancelledError());
    });
    cancelDisposable = registeredCancellation;
    if (settled) {
      disposeCancellation();
      return;
    }
    responseTimer = setTimeout(() => {
      failOnce(new DownloadTimeoutError("response"));
    }, timeouts.responseTimeoutMs);
    startHop(downloadUrl, maxRedirects);
  });
}

/**
 * Parse a SHA256 checksum file (sha256sum format) and extract the hash for a given asset.
 */
export function parseChecksumForAsset(checksumContent: string, assetName: string): string {
  const lines = checksumContent.split("\n");
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) {
      continue;
    }
    // sha256sum format: "hash  filename" or "hash *filename"
    const checksumMatch = /^([a-fA-F0-9]{64})\s+\*?(\S+)$/.exec(trimmed);
    if (checksumMatch && checksumMatch[2].trim() === assetName) {
      return checksumMatch[1].toLowerCase();
    }
  }
  throw new Error(`Checksum not found for asset "${assetName}" in downloaded checksum manifest`);
}

export function parseContentLengthHeader(headerValue: string | string[] | undefined): number {
  const rawValue = Array.isArray(headerValue) ? headerValue[0] : headerValue;
  return Number.parseInt(rawValue || "0", 10);
}

/**
 * Verify the SHA256 hash of a file.
 * Returns true if valid, throws on mismatch.
 */
export async function verifySha256(filePath: string, expectedHash: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash("sha256");
    const stream = fs.createReadStream(filePath);

    stream.on("data", (data) => hash.update(data));
    stream.on("end", () => {
      const actual = hash.digest("hex").toLowerCase();
      const expected = expectedHash.toLowerCase();
      if (actual === expected) {
        resolve();
      } else {
        reject(new Error(
          `SHA256 checksum mismatch. Expected: ${expected}, got: ${actual}. ` +
          "The downloaded file may be corrupted."
        ));
      }
    });
    stream.on("error", reject);
  });
}

/**
 * Verify a Cosign signature bundle using sigstore-js.
 *
 * Provenance verification is enforced: a genuine signature or
 * certificate-identity mismatch always rejects the binary. The one exception is
 * a Sigstore *infrastructure* failure (the bundled TUF trust root has expired,
 * or tuf-repo-cdn.sigstore.dev is unreachable), which is a Sigstore-side
 * problem, not a problem with the artifact, and it recurs every time the
 * embedded trust root ages out (~every few months). In that case, and only when
 * the binary's SHA-256 has already been verified against the release checksum
 * manifest (verifySha256 runs before this in the caller), we degrade to that
 * integrity guarantee with a loud warning rather than hard-blocking every
 * install until the extension ships a fresh trust root. Same posture as the
 * sqry.allowInsecureDownload air-gapped hatch, auto-triggered on a detected
 * Sigstore-infrastructure failure.
 */
export async function verifyCosignBundle(
  binaryPath: string,
  bundlePath: string,
  version: string,
  outputChannel: vscode.OutputChannel,
  storageUri: vscode.Uri,
  assetName?: string,
  expectedSha256?: string,
): Promise<void> {
  // Check if insecure download is allowed (hidden setting)
  const allowInsecure = vscode.workspace.getConfiguration("sqry").get<boolean>("allowInsecureDownload", false);

  if (allowInsecure) {
    outputChannel.appendLine(
      "[sqry] ⚠️ WARNING: Cosign signature verification SKIPPED because sqry.allowInsecureDownload is enabled. " +
      "This reduces security - the binary's provenance cannot be verified. " +
      "Only use this in air-gapped environments where Sigstore certificate transparency logs are unreachable."
    );
    return;
  }

  if (!fs.existsSync(bundlePath)) {
    throw new Error("Cosign signature bundle file is missing. Binary provenance cannot be verified.");
  }

  // Load the verifier separately so a load failure (e.g. sigstore's Node engine
  // floor is not met by this VS Code's runtime) is treated as an environment
  // problem, not an artifact problem. If the SHA-256 was already verified
  // against the release checksum manifest, degrade to that with a warning
  // rather than blocking the install.
  let sigstore: typeof import("sigstore");
  try {
    sigstore = await import("sigstore");
  } catch (loadError) {
    const detail = loadError instanceof Error ? loadError.message : String(loadError);
    if (!expectedSha256) {
      throw new Error(
        `Binary provenance could not be verified. Download rejected. ` +
        `The Sigstore verifier failed to load: ${detail}.`,
      );
    }
    outputChannel.appendLine(
      "[sqry] ⚠️ WARNING: the Sigstore verifier could not be loaded in this runtime " +
      `(${detail}). The binary's SHA-256 was verified against the release checksum manifest, so it is ` +
      "accepted on that basis. Update VS Code or the sqry extension to restore full provenance verification.",
    );
    return;
  }

  try {
    const bundleContent = JSON.parse(fs.readFileSync(bundlePath, "utf-8"));
    const hasEmbeddedDssePayload = isDsseAttestationBundle(bundleContent);
    if (hasEmbeddedDssePayload && assetName && expectedSha256) {
      verifyAttestationSubject(bundleContent, assetName, expectedSha256);
    }
    const identityCandidates = getCertificateIdentityCandidates(version);
    const proxyUrl = getConfiguredProxyUrl();
    const verifyOptions = buildSigstoreVerifyOptions(storageUri);

    fs.mkdirSync(verifyOptions.tufCachePath, { recursive: true });
    outputChannel.appendLine(`[sqry] Using Sigstore TUF cache: ${verifyOptions.tufCachePath}`);
    outputChannel.appendLine(`[sqry] Sigstore verification timeout: ${verifyOptions.timeout}ms`);
    if (proxyUrl) {
      outputChannel.appendLine("[sqry] Reusing VS Code http.proxy setting for Sigstore TUF fetches");
    }

    const verifyWithCandidates = async () => {
      await verifyCosignBundleWithIdentities(
        identityCandidates,
        outputChannel,
        async (certIdentity) => {
          await withTemporarySigstoreProxyEnv(proxyUrl, async () => {
            const options = {
              ...verifyOptions,
              certificateIssuer: OIDC_ISSUER,
              certificateIdentityURI: certIdentity,
            };
            if (hasEmbeddedDssePayload) {
              await sigstore.verify(bundleContent, options);
            } else {
              const binaryData = fs.readFileSync(binaryPath);
              await sigstore.verify(bundleContent, binaryData, options);
            }
          });
        },
      );
    };

    try {
      await verifyWithCandidates();
    } catch (error) {
      // A genuine signature/certificate-identity mismatch is never recoverable.
      if (!isRecoverableSigstoreTufError(error)) {
        throw error;
      }

      // First attempt hit a Sigstore trust-root/TUF error. Clear the (possibly
      // stale) cache and retry once against a freshly fetched root.
      outputChannel.appendLine(
        "[sqry] Sigstore trust-root/TUF error; clearing cache and retrying verification once",
      );
      clearSigstoreTufCache(verifyOptions.tufCachePath, outputChannel);
      fs.mkdirSync(verifyOptions.tufCachePath, { recursive: true });

      try {
        await verifyWithCandidates();
      } catch (retryError) {
        // Still a genuine mismatch after the retry -> reject.
        if (!isRecoverableSigstoreTufError(retryError)) {
          throw retryError;
        }
        // Persistent Sigstore-infrastructure failure (expired bundled trust
        // root, or the TUF CDN is unreachable). Without an independent
        // integrity anchor there is nothing to fall back to, so stay fatal.
        if (!expectedSha256) {
          throw retryError;
        }
        // The caller already verified this binary's SHA-256 against the release
        // checksum manifest, so accept it on that basis with a loud warning
        // instead of hard-blocking until a fresh trust root ships.
        const detail = retryError instanceof Error ? retryError.message : String(retryError);
        outputChannel.appendLine(
          "[sqry] ⚠️ WARNING: Sigstore transparency-log provenance could NOT be verified: " +
          `${detail}. This is a Sigstore infrastructure problem (an expired or unreachable trust root), ` +
          "not a bad binary. The binary's SHA-256 was verified against the release checksum manifest, so it " +
          "is accepted on that basis. Update the sqry extension to restore full provenance verification.",
        );
        return;
      }
    }

    outputChannel.appendLine("[sqry] Cosign signature verification succeeded - binary provenance confirmed");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const remedy = isRecoverableSigstoreTufError(error)
      ? " This is a Sigstore trust-root/TUF error (the transparency infrastructure could not be reached, or its root has rotated beyond the bundled trust root), not a problem with the binary itself. Update the sqry extension to pick up a current Sigstore root. If Sigstore is unreachable (air-gapped/offline), set `sqry.allowInsecureDownload: true` to fall back to SHA-256 checksum verification. Or install sqry yourself and set `sqry.path` to it with `sqry.autoDownload: false`."
      : " To bypass the download, install sqry and set `sqry.path` to it with `sqry.autoDownload: false`, or set `sqry.allowInsecureDownload: true` to fall back to SHA-256 checksum verification.";
    throw new Error(
      `Binary provenance could not be verified. Download rejected. ` +
      `Cosign verification failed: ${message}.` + remedy
    );
  }
}

export function verifyAttestationSubject(
  bundleContent: unknown,
  assetName: string,
  expectedSha256: string,
): void {
  const envelope = asRecord(bundleContent)["dsseEnvelope"];
  if (!isRecord(envelope)) {
    return;
  }

  const payload = envelope["payload"];
  if (typeof payload !== "string") {
    throw new Error("Sigstore DSSE bundle is missing its payload.");
  }

  let statement: unknown;
  try {
    statement = JSON.parse(Buffer.from(payload, "base64").toString("utf-8"));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`Sigstore DSSE payload could not be decoded: ${message}`);
  }

  const subjects = asRecord(statement)["subject"];
  if (!Array.isArray(subjects)) {
    throw new Error("Sigstore DSSE payload is missing its subject list.");
  }

  const expected = expectedSha256.toLowerCase();
  for (const subject of subjects) {
    if (!isRecord(subject) || subject["name"] !== assetName) {
      continue;
    }
    const digest = subject["digest"];
    if (!isRecord(digest) || typeof digest["sha256"] !== "string") {
      throw new Error(`Sigstore DSSE subject for ${assetName} is missing a sha256 digest.`);
    }
    const actual = digest["sha256"].toLowerCase();
    if (actual !== expected) {
      throw new Error(
        `Sigstore DSSE subject digest mismatch for ${assetName}. Expected: ${expected}, got: ${actual}.`,
      );
    }
    return;
  }

  throw new Error(`Sigstore DSSE bundle does not attest release asset ${assetName}.`);
}

export function isDsseAttestationBundle(bundleContent: unknown): boolean {
  return isRecord(asRecord(bundleContent)["dsseEnvelope"]);
}

export function isRecoverableSigstoreTufError(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /root was signed by 0\/\d+ keys|tuf|trusted root/i.test(message);
}

export function clearSigstoreTufCache(tufCachePath: string, outputChannel: vscode.OutputChannel): void {
  try {
    fs.rmSync(tufCachePath, { recursive: true, force: true });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel.appendLine(`[sqry] Failed to clear Sigstore TUF cache ${tufCachePath}: ${message}`);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function asRecord(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : {};
}

export function getCertificateIdentityCandidates(version: string): readonly string[] {
  const base = "https://github.com/verivus-oss/sqry/.github/workflows";
  return CERT_IDENTITY_WORKFLOWS.flatMap((workflow) => [
    `${base}/${workflow}@refs/tags/v${version}`,
    `${base}/${workflow}@refs/heads/main`,
  ]);
}

export async function verifyCosignBundleWithIdentities(
  identityCandidates: readonly string[],
  outputChannel: vscode.OutputChannel,
  verifier: (certificateIdentity: string) => Promise<void>,
): Promise<void> {
  const failures: string[] = [];

  for (const certIdentity of identityCandidates) {
    outputChannel.appendLine(
      `[sqry] Verifying Cosign bundle (issuer: ${OIDC_ISSUER}, identity: ${certIdentity})`,
    );
    try {
      await verifier(certIdentity);
      return;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      failures.push(`${certIdentity}: ${message}`);
    }
  }

  throw new Error(
    `Cosign verification failed for all allowlisted identities. Tried: ${failures.join(" | ")}`,
  );
}

async function downloadReleaseAsset(
  releaseBase: string,
  assetNames: readonly string[],
  destPath: string,
  outputChannel: vscode.OutputChannel,
  description: string,
  cancellationToken?: vscode.CancellationToken,
): Promise<string> {
  const failures: string[] = [];
  let sawOnlyMissingAssets = true;

  for (const assetName of assetNames) {
    outputChannel.appendLine(`[sqry] Downloading ${description}: ${releaseBase}/${assetName}`);
    try {
      await downloadWithProgress(
        `${releaseBase}/${assetName}`,
        destPath,
        undefined,
        cancellationToken,
      );
      return assetName;
    } catch (error) {
      cleanupFile(destPath);
      if (
        error instanceof DownloadCancelledError ||
        cancellationToken?.isCancellationRequested
      ) {
        throw error;
      }
      const message = error instanceof Error ? error.message : String(error);
      failures.push(`${assetName}: ${message}`);
      if (error instanceof ReleaseAssetUnavailableError) {
        continue;
      }
      sawOnlyMissingAssets = false;
      throw error;
    }
  }

  const errorMessage =
    `None of the expected ${description} assets were available. Tried: ${assetNames.join(", ")}. ` +
    `Errors: ${failures.join(" | ")}`;

  if (sawOnlyMissingAssets) {
    throw new ReleaseAssetUnavailableError(errorMessage);
  }

  throw new Error(errorMessage);
}

/**
 * Check for a previously downloaded binary that is still valid.
 * Runs `sqry --version` preflight to confirm it's executable.
 */
export async function findExistingBinary(
  storageUri: vscode.Uri,
  version: string,
  extensionMode = vscode.ExtensionMode.Production,
): Promise<string | null> {
  const platformInfo = detectPlatform();
  const versionCandidates = getDownloadVersionCandidates(version, extensionMode);

  for (const candidateVersion of versionCandidates) {
    const binDir = path.join(storageUri.fsPath, "bin", `v${candidateVersion}`);
    const binaryPath = path.join(binDir, platformInfo.binaryName);

    if (!fs.existsSync(binaryPath)) {
      continue;
    }

    // Verify it's executable via preflight check
    try {
      const { execFileSync } = await import("node:child_process");
      execFileSync(binaryPath, ["--version"], { timeout: 10000 });
      return binaryPath;
    } catch {
      // Continue searching lower patch versions in development mode.
    }
  }

  return null;
}

/**
 * Acquire a lockfile for download. Returns true if acquired, false if contention.
 */
export function acquireLock(storageUri: vscode.Uri): boolean {
  const lockPath = path.join(storageUri.fsPath, "download.lock");

  // Check for stale lock
  if (fs.existsSync(lockPath)) {
    try {
      const stat = fs.statSync(lockPath);
      const age = Date.now() - stat.mtimeMs;
      if (age < LOCK_STALE_MS) {
        return false; // Lock is held by another window
      }
      // Stale lock - clean it up
      fs.unlinkSync(lockPath);
    } catch {
      // If we can't stat, try to proceed
    }
  }

  // Create lock
  fs.mkdirSync(path.dirname(lockPath), { recursive: true });
  fs.writeFileSync(lockPath, `${process.pid}\n${Date.now()}`);
  return true;
}

/**
 * Release the download lockfile.
 */
export function releaseLock(storageUri: vscode.Uri): void {
  const lockPath = path.join(storageUri.fsPath, "download.lock");
  cleanupFile(lockPath);
}

/**
 * Remove old version directories, keeping the N most recent.
 */
export function cleanupOldVersions(storageUri: vscode.Uri, currentVersion: string): void {
  const binDir = path.join(storageUri.fsPath, "bin");
  if (!fs.existsSync(binDir)) {
    return;
  }

  const entries = fs.readdirSync(binDir)
    .filter((entry) => entry.startsWith("v"))
    .sort((a, b) => a.localeCompare(b))
    .reverse();

  // Always keep current version + N-1 for rollback
  const toKeep = new Set<string>();
  toKeep.add(`v${currentVersion}`);

  let kept = 0;
  for (const entry of entries) {
    if (toKeep.has(entry)) {
      continue;
    }
    if (kept < KEEP_VERSIONS - 1) {
      toKeep.add(entry);
      kept++;
    }
  }

  for (const entry of entries) {
    if (!toKeep.has(entry)) {
      const dirPath = path.join(binDir, entry);
      try {
        fs.rmSync(dirPath, { recursive: true, force: true });
      } catch {
        // Best-effort cleanup
      }
    }
  }
}

/**
 * Find the most recent previous version binary for rollback.
 */
async function findRollbackBinary(storageUri: vscode.Uri, currentVersion: string): Promise<string | null> {
  const binDir = path.join(storageUri.fsPath, "bin");
  if (!fs.existsSync(binDir)) {
    return null;
  }

  const platformInfo = detectPlatform();
  const entries = fs.readdirSync(binDir)
    .filter((entry) => entry.startsWith("v") && entry !== `v${currentVersion}`)
    .sort((a, b) => a.localeCompare(b))
    .reverse();

  for (const entry of entries) {
    const binaryPath = path.join(binDir, entry, platformInfo.binaryName);
    if (fs.existsSync(binaryPath)) {
      try {
        const { execFileSync } = await import("node:child_process");
        execFileSync(binaryPath, ["--version"], { timeout: 10000 });
        return binaryPath;
      } catch {
        continue;
      }
    }
  }

  return null;
}

/**
 * Extract the launch binary and its bundled runtime DLLs from a verified
 * archive into `destDir` (flattened). Only the member binary and `.dll` files
 * are written; the other bundled executables are skipped to save disk.
 */
export function extractArchiveMembers(
  archivePath: string,
  destDir: string,
  memberBinary: string,
  outputChannel: vscode.OutputChannel,
): void {
  const entries = unzipSync(new Uint8Array(fs.readFileSync(archivePath)));
  let wroteBinary = false;
  for (const [name, content] of Object.entries(entries)) {
    if (content.length === 0) {
      continue; // directory entry
    }
    const base = path.basename(name);
    const isBinary = base === memberBinary;
    const isDll = base.toLowerCase().endsWith(".dll");
    if (!isBinary && !isDll) {
      continue;
    }
    fs.writeFileSync(path.join(destDir, base), Buffer.from(content));
    if (isBinary) {
      wroteBinary = true;
    }
  }
  if (!wroteBinary) {
    throw new Error(`Archive did not contain the expected binary "${memberBinary}".`);
  }
  outputChannel.appendLine(`[sqry] Extracted ${memberBinary} + bundled DLLs from archive`);
}

/**
 * Main download orchestrator. Downloads, verifies, and installs the sqry binary.
 * Returns the path to the installed binary.
 */
export async function downloadBinary(
  context: vscode.ExtensionContext,
  outputChannel: vscode.OutputChannel,
  cancellationToken?: vscode.CancellationToken,
): Promise<string> {
  const storageUri = context.globalStorageUri;
  const requestedVersion = getBinaryVersion();
  const platformInfo = detectPlatform();
  const versionCandidates = getDownloadVersionCandidates(requestedVersion, context.extensionMode);

  // Acquire lock
  if (!acquireLock(storageUri)) {
    throw new Error("Another VS Code window is already downloading sqry. Please wait.");
  }

  try {
    let lastMissingAssetError: ReleaseAssetUnavailableError | null = null;

    for (const effectiveVersion of versionCandidates) {
      const versionDir = path.join(storageUri.fsPath, "bin", `v${effectiveVersion}`);
      const finalBinaryPath = path.join(versionDir, platformInfo.binaryName);
      // Resolve the asset name (the Windows zip embeds the version) and decide
      // whether we download a bare binary or an archive to verify+extract.
      const assetName = platformInfo.asset.replace("{version}", effectiveVersion);
      const isArchive = platformInfo.archive !== undefined;
      const tmpDownloadPath = isArchive
        ? path.join(versionDir, "artifact.tmp")
        : `${finalBinaryPath}.tmp`;
      const tmpBundlePath = path.join(versionDir, "attestation.tmp");
      const tmpChecksumPath = path.join(versionDir, "checksums.tmp");

      try {
        if (effectiveVersion !== requestedVersion) {
          outputChannel.appendLine(
            `[sqry] Requested binary v${requestedVersion} is not public yet; trying compatible dev fallback v${effectiveVersion}`,
          );
        }

        // Ensure directories exist
        fs.mkdirSync(versionDir, { recursive: true });

        const releaseBase = `${GITHUB_RELEASE_BASE}/v${effectiveVersion}`;

        // Step 1: Download checksum manifest
        const checksumAssetName = await downloadReleaseAsset(
          releaseBase,
          getChecksumAssetCandidates(),
          tmpChecksumPath,
          outputChannel,
          "checksum manifest",
          cancellationToken,
        );
        const checksumContent = fs.readFileSync(tmpChecksumPath, "utf-8");
        cleanupFile(tmpChecksumPath);
        outputChannel.appendLine(`[sqry] Parsed checksums from ${checksumAssetName}`);

        // Step 2: Parse expected hash
        const expectedHash = parseChecksumForAsset(checksumContent, assetName);
        outputChannel.appendLine(`[sqry] Expected SHA256 for ${assetName}: ${expectedHash}`);

        // Step 3: Download the asset with progress
        outputChannel.appendLine(`[sqry] Downloading: ${assetName}`);
        await downloadWithProgress(
          `${releaseBase}/${assetName}`,
          tmpDownloadPath,
          (downloaded, total) => {
            const pct = Math.round((downloaded / total) * 100);
            outputChannel.appendLine(`[sqry] Download progress: ${pct}%`);
          },
          cancellationToken,
        );

        // Step 4: Verify SHA256 (of the downloaded asset: binary or archive)
        outputChannel.appendLine("[sqry] Verifying SHA256 checksum...");
        try {
          await verifySha256(tmpDownloadPath, expectedHash);
        } catch (err) {
          cleanupFile(tmpDownloadPath);
          throw err;
        }
        outputChannel.appendLine("[sqry] SHA256 checksum verified");

        // Step 5: Download attestation bundle
        const bundleAssetName = await downloadReleaseAsset(
          releaseBase,
          getBundleAssetCandidates(assetName),
          tmpBundlePath,
          outputChannel,
          "attestation bundle",
          cancellationToken,
        );
        const finalBundlePath = path.join(versionDir, bundleAssetName);
        outputChannel.appendLine(`[sqry] Downloaded attestation bundle: ${bundleAssetName}`);

        // Step 6: Verify Cosign bundle (MANDATORY)
        outputChannel.appendLine("[sqry] Verifying Cosign signature bundle...");
        try {
          await verifyCosignBundle(
            tmpDownloadPath,
            tmpBundlePath,
            effectiveVersion,
            outputChannel,
            storageUri,
            assetName,
            expectedHash,
          );
        } catch (err) {
          cleanupFile(tmpDownloadPath);
          cleanupFile(tmpBundlePath);
          throw err;
        }

        // Step 7: Install to final paths. For an archive, extract the launch
        // binary + bundled DLLs; for a bare binary, atomically rename it.
        if (isArchive && platformInfo.archive) {
          try {
            extractArchiveMembers(
              tmpDownloadPath,
              versionDir,
              platformInfo.archive.memberBinary,
              outputChannel,
            );
          } catch (err) {
            cleanupFile(tmpDownloadPath);
            cleanupFile(tmpBundlePath);
            throw err;
          }
          cleanupFile(tmpDownloadPath);
        } else {
          fs.renameSync(tmpDownloadPath, finalBinaryPath);
        }
        fs.renameSync(tmpBundlePath, finalBundlePath);

        // Step 8: Set executable permissions on unix
        applyBinaryPermissions(finalBinaryPath);

        // Step 9: Preflight check
        outputChannel.appendLine("[sqry] Running preflight check (sqry --version)...");
        try {
          const { execFileSync } = await import("node:child_process");
          const versionOutput = execFileSync(finalBinaryPath, ["--version"], {
            timeout: 10000,
            encoding: "utf-8",
          });
          outputChannel.appendLine(`[sqry] Preflight OK: ${versionOutput.trim()}`);
        } catch (preflightError) {
          const preflightMessage = describePreflightError(preflightError);
          outputChannel.appendLine(`[sqry] Preflight check failed (${preflightMessage}), attempting rollback...`);

          const rollbackPath = await findRollbackBinary(storageUri, effectiveVersion);
          if (rollbackPath) {
            outputChannel.appendLine(`[sqry] Rolling back to: ${rollbackPath}`);
            return rollbackPath;
          }

          throw new Error(
            "Downloaded binary failed preflight check (sqry --version) and no previous version is available for rollback."
          );
        }

        // Step 10: Write version.json
        const versionInfo: VersionInfo = {
          version: effectiveVersion,
          downloadedAt: new Date().toISOString(),
          platform: `${process.platform}-${process.arch}`,
          sha256: expectedHash,
          ...(effectiveVersion === requestedVersion ? {} : { requestedVersion }),
        };
        fs.writeFileSync(
          path.join(storageUri.fsPath, "bin", "version.json"),
          JSON.stringify(versionInfo, null, 2),
        );

        // Step 11: Cleanup old versions
        cleanupOldVersions(storageUri, effectiveVersion);

        outputChannel.appendLine(`[sqry] Binary installed successfully at ${finalBinaryPath}`);
        return finalBinaryPath;
      } catch (error) {
        cleanupFile(tmpDownloadPath);
        cleanupFile(tmpBundlePath);
        cleanupFile(tmpChecksumPath);

        if (error instanceof ReleaseAssetUnavailableError && effectiveVersion !== versionCandidates[versionCandidates.length - 1]) {
          lastMissingAssetError = error;
          outputChannel.appendLine(`[sqry] Release assets for v${effectiveVersion} are unavailable: ${error.message}`);
          continue;
        }

        throw error;
      }
    }

    if (isDevelopmentLikeMode(context.extensionMode)) {
      throw new Error(
        `No published sqry patch release is available for requested development version v${requestedVersion} in the same major.minor line. ` +
        (lastMissingAssetError ? `Last error: ${lastMissingAssetError.message}` : "")
      );
    }

    throw new Error(
      `Public release assets for sqry v${requestedVersion} are not available. ` +
      "Auto-download requires an exact published binaryVersion."
    );
  } catch (error) {
    throw error;
  } finally {
    releaseLock(storageUri);
  }
}

export function applyBinaryPermissions(
  finalBinaryPath: string,
  platform: NodeJS.Platform = process.platform,
): void {
  if (platform !== "win32") {
    // The downloaded binary lives in the extension's private storage area.
    // Restrict execution to the current user instead of granting world access.
    fs.chmodSync(finalBinaryPath, 0o700);
  }
}

export function describePreflightError(preflightError: unknown): string {
  return preflightError instanceof Error ? preflightError.message : String(preflightError);
}

function cleanupFile(filePath: string): void {
  try {
    if (fs.existsSync(filePath)) {
      fs.unlinkSync(filePath);
    }
  } catch (_) {
    // Best-effort cleanup — failure to remove temp files is non-fatal
  }
}
