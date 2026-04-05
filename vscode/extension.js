// @ts-check
const { LanguageClient } = require("vscode-languageclient/node");
const vscode = require("vscode");

module.exports = {
  /** @param {import("vscode").ExtensionContext} context*/
  activate(context) {
    let log = vscode.window.createOutputChannel("Prolog LSP");

    let command = context.asAbsolutePath("lsp_bin/prolog-lsp");

    /** @type {import("vscode-languageclient/node").ServerOptions} */
    const serverOptions = {
      run: {
        command,
        options: {
          env: {
            RUST_BACKTRACE: "1",
            RUST_LOG: "INFO",
          },
        },
      },
      debug: {
        command,
        args: [],
        options: {
          env: {
            RUST_BACKTRACE: "1",
            RUST_LOG: "INFO",
          },
        },
      },
    };

    /** @type {import("vscode-languageclient/node").LanguageClientOptions} */
    const clientOptions = {
      documentSelector: [{ language: "prolog" }],
      outputChannel: log,
      traceOutputChannel: log,
    };

    const client = new LanguageClient(
      "prolog-lsp",
      "Prolog Language Server",
      serverOptions,
      clientOptions,
    );

    log.appendLine("Starting Prolog LSP...");
    client.start();
  },
};
