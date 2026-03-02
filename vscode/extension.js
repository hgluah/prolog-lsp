// @ts-check
const { LanguageClient } = require("vscode-languageclient/node");

module.exports = {
  /** @param {import("vscode").ExtensionContext} context*/
  activate(context) {
    /** @type {import("vscode-languageclient/node").ServerOptions} */
    const serverOptions = {
      run: {
        command: "prolog-lsp",
      },
      debug: {
        command: "prolog-lsp",
        args: [],
      },
    };

    /** @type {import("vscode-languageclient/node").LanguageClientOptions} */
    const clientOptions = {
      documentSelector: [{ language: "html", pattern: ".*{pl}" }],
    };

    const client = new LanguageClient(
      "prolog-lsp",
      "Prolog Language Server",
      serverOptions,
      clientOptions,
    );

    client.start();
  },
};
