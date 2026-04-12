import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as https from "node:https";
import * as path from "node:path";
import * as url from "node:url";
import * as vscode from "vscode";

const GITHUB_RELEASE_BASE = "https://github.com/verivus-oss/sqry/releases/download";
const OIDC_ISSUER = "https://token.actions.githubusercontent.com";
const CERT_IDENTITY_TAG_PREFIX = "https://github.com/verivus-oss/sqry/.github/workflows/oss-distribute.yml@refs/tags/v";
const CERT_IDENTITY_MAIN = "https://github.com/verivus-oss/sqry/.github/workflows/oss-distribute.yml@refs/heads/main";
const MAX_DOWNLOAD_SIZE = 200 * 1024 * 1024; // 200 MB
const LOCK_STALE_MS = 10 * 60 * 1000; // 10 minutes
const KEEP_VERSIONS = 2;
const SIGSTORE_VERIFY_TIMEOUT_MS = 30_000;
const SIGSTORE_TUF_CACHE_DIR = "sigstore-tuf-cache";
const CHECKSUM_ASSET_NAMES = ["SHA256SUMS.txt", "CHECKSUMS.sha256"] as const;
const ATTESTATION_BUNDLE_NAMES = ["release-artifacts.attestation.json"] as const;

class ReleaseAssetUnavailableError extends Error {}

export interface PlatformInfo {
  asset: string;
  binaryName: string;
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

  if (platform === "linux" && arch === "x64") {
    return { asset: "sqry-linux-x86_64", binaryName: "sqry" };
  }
  if (platform === "win32" && arch === "x64") {
    return { asset: "sqry-windows-x86_64.exe", binaryName: "sqry.exe" };
  }
  if (platform === "darwin" && arch === "arm64") {
    return { asset: "sqry-macos-arm64", binaryName: "sqry" };
  }
  if (platform === "darwin" && arch === "x64") {
    throw new Error(
      "sqry does not provide pre-built binaries for macOS Intel (x86_64). " +
      "Install via: cargo install sqry-cli"
    );
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
): Promise<void> {
  return new Promise((resolve, reject) => {
    if (cancellationToken?.isCancellationRequested) {
      reject(new Error("Download cancelled"));
      return;
    }

    const parsed = new url.URL(downloadUrl);

    if (parsed.protocol !== "https:") {
      reject(new Error(`Refusing non-HTTPS download URL: ${downloadUrl}`));
      return;
    }

    if (!isAllowedHost(parsed.hostname)) {
      reject(new Error(`Download host not in allowlist: ${parsed.hostname}`));
      return;
    }

    const proxyUrl = getConfiguredProxyUrl();

    let requestOptions: https.RequestOptions = {
      hostname: parsed.hostname,
      path: parsed.pathname + parsed.search,
      headers: { "User-Agent": "sqry-vscode" },
    };

    if (proxyUrl) {
      try {
        // Dynamic require to allow the module to load even without the dep
        // eslint-disable-next-line @typescript-eslint/no-var-requires
        const { HttpsProxyAgent } = require("https-proxy-agent");
        requestOptions.agent = new HttpsProxyAgent(proxyUrl);
      } catch {
        // Fall through without proxy if agent creation fails
      }
    }

    const req = https.get(requestOptions, (res) => {
      // Handle redirects
      if (res.statusCode && res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume(); // Drain the response

        if (maxRedirects <= 0) {
          reject(new Error("Too many redirects"));
          return;
        }

        const redirectUrl = res.headers.location;
        const redirectParsed = new url.URL(redirectUrl);

        if (redirectParsed.protocol !== "https:") {
          reject(new Error(`Refusing non-HTTPS redirect to: ${redirectUrl}`));
          return;
        }

        if (!isAllowedHost(redirectParsed.hostname)) {
          reject(new Error(`Redirect host not in allowlist: ${redirectParsed.hostname}`));
          return;
        }

        downloadWithProgress(redirectUrl, destPath, onProgress, cancellationToken, maxRedirects - 1)
          .then(resolve)
          .catch(reject);
        return;
      }

      if (res.statusCode === 404) {
        res.resume();
        reject(new ReleaseAssetUnavailableError("HTTP 404: Release asset not found"));
        return;
      }

      if (!res.statusCode || res.statusCode >= 400) {
        res.resume();
        reject(new Error(`HTTP error: ${res.statusCode}`));
        return;
      }

      const contentLength = parseContentLengthHeader(res.headers["content-length"]);
      if (contentLength > MAX_DOWNLOAD_SIZE) {
        res.resume();
        reject(new Error(`Download rejected: file exceeds expected size limit (${contentLength} bytes > ${MAX_DOWNLOAD_SIZE} bytes)`));
        return;
      }

      const fileStream = fs.createWriteStream(destPath);
      let downloaded = 0;

      const onCancel = cancellationToken?.onCancellationRequested(() => {
        res.destroy();
        fileStream.destroy();
        cleanupFile(destPath);
        reject(new Error("Download cancelled"));
      });

      res.on("data", (chunk: Buffer) => {
        downloaded += chunk.length;
        if (downloaded > MAX_DOWNLOAD_SIZE) {
          res.destroy();
          fileStream.destroy();
          cleanupFile(destPath);
          reject(new Error(`Download rejected: file exceeds expected size limit`));
          return;
        }
        if (onProgress && contentLength > 0) {
          onProgress(downloaded, contentLength);
        }
      });

      res.pipe(fileStream);

      fileStream.on("finish", () => {
        onCancel?.dispose();
        resolve();
      });

      fileStream.on("error", (err) => {
        onCancel?.dispose();
        cleanupFile(destPath);
        reject(err);
      });

      res.on("error", (err) => {
        onCancel?.dispose();
        fileStream.destroy();
        cleanupFile(destPath);
        reject(err);
      });
    });

    req.on("error", (err) => {
      reject(new Error(`Network error: ${err.message}. Check your internet connection and proxy settings.`));
    });
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
 * This is MANDATORY - failure means the binary is rejected.
 */
export async function verifyCosignBundle(
  binaryPath: string,
  bundlePath: string,
  version: string,
  outputChannel: vscode.OutputChannel,
  storageUri: vscode.Uri,
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

  try {
    const sigstore = await import("sigstore");
    const bundleContent = JSON.parse(fs.readFileSync(bundlePath, "utf-8"));
    const binaryData = fs.readFileSync(binaryPath);
    const identityCandidates = getCertificateIdentityCandidates(version);
    const proxyUrl = getConfiguredProxyUrl();
    const verifyOptions = buildSigstoreVerifyOptions(storageUri);

    fs.mkdirSync(verifyOptions.tufCachePath, { recursive: true });
    outputChannel.appendLine(`[sqry] Using Sigstore TUF cache: ${verifyOptions.tufCachePath}`);
    outputChannel.appendLine(`[sqry] Sigstore verification timeout: ${verifyOptions.timeout}ms`);
    if (proxyUrl) {
      outputChannel.appendLine("[sqry] Reusing VS Code http.proxy setting for Sigstore TUF fetches");
    }

    await verifyCosignBundleWithIdentities(
      identityCandidates,
      outputChannel,
      async (certIdentity) => {
        await withTemporarySigstoreProxyEnv(proxyUrl, async () => {
          await sigstore.verify(bundleContent, binaryData, {
            ...verifyOptions,
            certificateIssuer: OIDC_ISSUER,
            certificateIdentityURI: certIdentity,
          });
        });
      },
    );

    outputChannel.appendLine("[sqry] Cosign signature verification succeeded - binary provenance confirmed");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(
      `Binary provenance could not be verified. Download rejected. ` +
      `Cosign verification failed: ${message}`
    );
  }
}

export function getCertificateIdentityCandidates(version: string): readonly string[] {
  return [
    `${CERT_IDENTITY_TAG_PREFIX}${version}`,
    CERT_IDENTITY_MAIN,
  ];
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
      const message = error instanceof Error ? error.message : String(error);
      failures.push(`${assetName}: ${message}`);
      if (!(error instanceof ReleaseAssetUnavailableError)) {
        sawOnlyMissingAssets = false;
      }
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
      const tmpBinaryPath = `${finalBinaryPath}.tmp`;
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
        const expectedHash = parseChecksumForAsset(checksumContent, platformInfo.asset);
        outputChannel.appendLine(`[sqry] Expected SHA256 for ${platformInfo.asset}: ${expectedHash}`);

        // Step 3: Download binary with progress
        outputChannel.appendLine(`[sqry] Downloading binary: ${platformInfo.asset}`);
        await downloadWithProgress(
          `${releaseBase}/${platformInfo.asset}`,
          tmpBinaryPath,
          (downloaded, total) => {
            const pct = Math.round((downloaded / total) * 100);
            outputChannel.appendLine(`[sqry] Download progress: ${pct}%`);
          },
          cancellationToken,
        );

        // Step 4: Verify SHA256
        outputChannel.appendLine("[sqry] Verifying SHA256 checksum...");
        try {
          await verifySha256(tmpBinaryPath, expectedHash);
        } catch (err) {
          cleanupFile(tmpBinaryPath);
          throw err;
        }
        outputChannel.appendLine("[sqry] SHA256 checksum verified");

        // Step 5: Download attestation bundle
        const bundleAssetName = await downloadReleaseAsset(
          releaseBase,
          getBundleAssetCandidates(platformInfo.asset),
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
          await verifyCosignBundle(tmpBinaryPath, tmpBundlePath, effectiveVersion, outputChannel, storageUri);
        } catch (err) {
          cleanupFile(tmpBinaryPath);
          cleanupFile(tmpBundlePath);
          throw err;
        }

        // Step 7: Atomic rename to final paths
        fs.renameSync(tmpBinaryPath, finalBinaryPath);
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
        cleanupFile(tmpBinaryPath);
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
