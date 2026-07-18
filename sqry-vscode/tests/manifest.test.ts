import { expect } from "chai";
import * as fs from "node:fs";
import * as path from "node:path";

interface CommandContribution {
  readonly command: string;
  readonly title?: string;
  readonly enablement?: string;
}

interface MenuContribution {
  readonly command: string;
  readonly when?: string;
}

interface ViewWelcomeContribution {
  readonly view: string;
  readonly when?: string;
}

interface ConfigurationBlock {
  readonly properties?: Record<string, { readonly scope?: string }>;
}

interface Manifest {
  readonly contributes: {
    readonly commands: CommandContribution[];
    readonly configuration: ConfigurationBlock | ConfigurationBlock[];
    readonly menus: {
      readonly "view/title": MenuContribution[];
    };
    readonly viewsWelcome: ViewWelcomeContribution[];
  };
}

function loadManifest(): Manifest {
  const manifestPath = path.join(__dirname, "..", "package.json");
  return JSON.parse(fs.readFileSync(manifestPath, "utf8")) as Manifest;
}

function loadVsCodeIgnore(): string[] {
  const ignorePath = path.join(__dirname, "..", ".vscodeignore");
  return fs.readFileSync(ignorePath, "utf8").split(/\r?\n/);
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

  it("keeps View Logs available while readiness-gating every late command", () => {
    for (const command of manifest.contributes.commands) {
      if (command.command === "sqry.showOutput") {
        expect(command.enablement).to.equal(undefined);
        continue;
      }
      expect(
        command.enablement,
        `${command.command} must be unavailable until sqry startup is Ready`,
      ).to.equal("sqry.ready");
    }
  });

  it("hides every late search-results title action until sqry is Ready", () => {
    const titleActions = manifest.contributes.menus["view/title"];
    const expectedCommands = new Set([
      "sqry.searchWorkspace",
      "sqry.query",
      "sqry.refreshStats",
      "sqry.clearResults",
      "sqry.filterResults",
      "sqry.sortResults",
    ]);

    expect(new Set(titleActions.map((action) => action.command))).to.deep.equal(expectedCommands);
    for (const action of titleActions) {
      expect(
        action.when,
        `${action.command} must be hidden during the activation window`,
      ).to.include("sqry.ready");
    }
  });

  it("does not expose startup command links in the view welcome during loading", () => {
    expect(manifest.contributes.viewsWelcome).to.have.length(1);
    expect(manifest.contributes.viewsWelcome[0].view).to.equal("sqry.searchResults");
    expect(manifest.contributes.viewsWelcome[0].when).to.equal("sqry.ready");
  });

  it("scopes sqry.path as machine-overridable so it does not sync across machines", () => {
    // A machine-specific binary path (e.g. ~/.local/bin/sqry on Linux) must
    // not ride Settings Sync onto a Windows machine, where it is invalid.
    // `machine-overridable` matches the sibling sqry.indexRoot setting.
    const sqryPath = properties["sqry.path"];
    expect(sqryPath, "sqry.path property must be declared").to.not.equal(undefined);
    expect(sqryPath.scope).to.equal("machine-overridable");
  });

  it("excludes generated sqry analysis indexes from the VSIX", () => {
    // Dogfooding creates `.sqry/` next to extension sources. It is local
    // analysis state and must never inflate or leak into a Marketplace VSIX.
    expect(loadVsCodeIgnore()).to.include(".sqry/");
  });
});
