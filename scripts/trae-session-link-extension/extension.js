const vscode = require("vscode");

const COMMAND = "_workbench.openRuntimeWindow";

function activate(context) {
  const output = vscode.window.createOutputChannel("Session Finder Link Demo");
  context.subscriptions.push(output);
  context.subscriptions.push(vscode.window.registerUriHandler({
    async handleUri(uri) {
      if (uri.path !== "/open") return;
      const params = new URLSearchParams(uri.query);
      const sessionId = params.get("id");
      const cwd = params.get("cwd");
      if (!sessionId || !/^[a-f0-9]{24}$/i.test(sessionId)) {
        vscode.window.showErrorMessage("Session Finder: invalid TRAE conversation ID.");
        return;
      }
      if (!cwd || !require("path").isAbsolute(cwd) || !require("fs").statSync(cwd, { throwIfNoEntry: false })?.isDirectory()) {
        vscode.window.showErrorMessage("Session Finder: an existing absolute project directory is required.");
        return;
      }
      const commands = await vscode.commands.getCommands(false);
      if (!commands.includes(COMMAND)) {
        output.appendLine("Unsupported window: Agent handoff command is unavailable.");
        output.show();
        vscode.window.showErrorMessage(
          "Session Finder: this TRAE window does not expose the Agent handoff command."
        );
        return;
      }
      try {
        const user = await vscode.commands.executeCommand("icube.common.getUserInfo");
        if (!user?.userId) throw new Error("Sign in to TRAE before opening a conversation.");
        const result = await vscode.commands.executeCommand(COMMAND, "Agent", {
          kind: "task",
          task: {
            sessionId,
            format: "chat-core",
            accountScope: String(user.userId),
            dataSource: "local",
            workspace: { kind: "folder", uri: vscode.Uri.file(cwd).toJSON() }
          }
        });
        output.appendLine(`Agent handoff committed: ${result === true}`);
        if (result !== true) {
          output.show();
          vscode.window.showWarningMessage(
            "Session Finder: TRAE did not commit the Agent handoff; conversation opening is unconfirmed."
          );
        }
      } catch (error) {
        output.appendLine(`Open failed: ${error.message}`);
        output.show();
        vscode.window.showErrorMessage(`Session Finder: ${error.message}`);
      }
    }
  }));
}

exports.activate = activate;
