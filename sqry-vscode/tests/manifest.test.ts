import { expect } from "chai";
import * as fs from "node:fs";
import * as path from "node:path";

interface CommandContribution {
  readonly command: string;
  readonly title?: string;
}

interface ConfigurationBlock {
  readonly properties?: Record<string, { readonly scope?: string }>;
}

interface Manifest {
  readonly contributes: {
    readonly commands: CommandContribution[];
    readonly configuration: ConfigurationBlock | ConfigurationBlock[];
  };
}

function loadManifest(): Manifest {
  const manifestPath = path.join(__dirname, "..", "package.json");
  return JSON.parse(fs.readFileSync(manifestPath, "utf8")) as Manifest;
}

/** Flatten `contributes.configuration` (object or array form) into one property map. */
function configProperties(
  configuration: ConfigurationBlock | ConfigurationBlock[],
): Record<string, { readonly scope?: string }> {
  const blocks = Array.isArray(configuration) ? configuration : [configuration];
  return Object.assign({}, ...blocks.map((b) => b.properties ?? {}));
}

describe("package.json manifest", () => {
  const manifest = loadManifest();
  const declaredCommands = new Set(manifest.contributes.commands.map((c) => c.command));
  const properties = configProperties(manifest.contributes.configuration);

  it("declares sqry.showOutput (the failure UI 'View Logs' target)", () => {
    // The status bar (statusBar.ts) and the search panel 'View Logs' action
    // (searchPanel.ts) both invoke `sqry.showOutput`. It must be a declared
    // command AND registered early enough that the failure UI can call it
    // even when activation bails out on a binary-resolution failure
    // (extension.ts registers it before the LSP-start early return).
    expect(declaredCommands.has("sqry.showOutput")).to.equal(true);
  });

  it("scopes sqry.path as machine-overridable so it does not sync across machines", () => {
    // A machine-specific binary path (e.g. ~/.local/bin/sqry on Linux) must
    // not ride Settings Sync onto a Windows machine, where it is invalid.
    // `machine-overridable` matches the sibling sqry.indexRoot setting.
    const sqryPath = properties["sqry.path"];
    expect(sqryPath, "sqry.path property must be declared").to.not.equal(undefined);
    expect(sqryPath.scope).to.equal("machine-overridable");
  });
});
