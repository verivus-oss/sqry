import * as vscode from "vscode";

export class SqryCodeActionProvider implements vscode.CodeActionProvider {
  public static readonly providedCodeActionKinds = [vscode.CodeActionKind.QuickFix];

  provideCodeActions(
    _document: vscode.TextDocument,
    _range: vscode.Range | vscode.Selection,
    context: vscode.CodeActionContext,
  ): vscode.CodeAction[] {
    const actions: vscode.CodeAction[] = [];

    for (const diagnostic of context.diagnostics) {
      if (diagnostic.source !== "sqry") { continue; }

      switch (diagnostic.code) {
        case "sqry:unused": {
          // Extract symbol name from message: "'symbolName' appears to be unused"
          const match = /^'(.+)' appears to be unused$/.exec(diagnostic.message);
          if (match) {
            const action = new vscode.CodeAction(
              `Show callers of '${match[1]}' in sqry`,
              vscode.CodeActionKind.QuickFix,
            );
            action.command = {
              command: "sqry.runQueryInternal",
              title: "Show callers",
              arguments: [`callers:${match[1]}`],
            };
            action.diagnostics = [diagnostic];
            actions.push(action);
          }
          break;
        }
        case "sqry:cycle": {
          const action = new vscode.CodeAction(
            "Show cycle path in sqry",
            vscode.CodeActionKind.QuickFix,
          );
          // Extract cycle members from message
          const cycleMatch = /circular dependency: (.+)$/.exec(diagnostic.message);
          if (cycleMatch) {
            action.command = {
              command: "sqry.runQueryInternal",
              title: "Show cycle",
              arguments: [cycleMatch[1]],
            };
          }
          action.diagnostics = [diagnostic];
          actions.push(action);
          break;
        }
        case "sqry:duplicate": {
          // Navigate to the first related location
          if (diagnostic.relatedInformation && diagnostic.relatedInformation.length > 0) {
            const related = diagnostic.relatedInformation[0];
            // The related location comes from duplicate-detection results. Only
            // offer navigation for a `file:` URI, and route it through the guarded
            // `sqry.openResultFile` command (its fsPath is passed as data; the
            // guard rejects out-of-workspace targets) rather than opening the raw
            // URI. A non-`file` scheme is refused here at the source.
            if (related.location.uri.scheme === "file") {
              const action = new vscode.CodeAction(
                "Navigate to duplicate",
                vscode.CodeActionKind.QuickFix,
              );
              const relatedRange = related.location.range;
              action.command = {
                command: "sqry.openResultFile",
                title: "Open duplicate",
                arguments: [
                  related.location.uri.fsPath,
                  {
                    startLine: relatedRange.start.line,
                    startCharacter: relatedRange.start.character,
                    endLine: relatedRange.end.line,
                    endCharacter: relatedRange.end.character,
                  },
                ],
              };
              action.diagnostics = [diagnostic];
              actions.push(action);
            }
          }
          break;
        }
        default:
          break;
      }
    }

    return actions;
  }
}
