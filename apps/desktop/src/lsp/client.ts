import { commands, events, type LspError, type LspServer, type LspSession } from "../bindings";

type Json = Record<string, unknown>;
type Pending = { resolve: (result: unknown) => void; reject: (error: Error) => void };
type Handler = (params: Json) => void;

/** The script files an editor shows, and the server each one talks to. */
export function serverFor(file: string): LspServer | null {
  switch (file) {
    case "keyboard.luau":
      return "luau_keyboard";
    case "host.luau":
      return "luau_host";
    case "options.luau":
      return "luau_options";
    case "bridge.luau":
      return "luau_bridge";
    case "canvas.slang":
      return "slang";
    default:
      return null;
  }
}

export function describeLspError(error: LspError): string {
  switch (error.kind) {
    case "unavailable":
      return error.reason;
    case "library":
      return `the package can't be read (${error.error.kind})`;
    case "no_session":
      return "the language server has stopped";
    case "io":
      return error.reason;
  }
}

const CLIENT_CAPABILITIES = {
  textDocument: {
    synchronization: { dynamicRegistration: false, didSave: false },
    publishDiagnostics: { relatedInformation: false },
    completion: { completionItem: { snippetSupport: true, documentationFormat: ["markdown", "plaintext"] }, contextSupport: true },
    hover: { contentFormat: ["markdown", "plaintext"] },
    signatureHelp: { signatureInformation: { documentationFormat: ["markdown", "plaintext"], parameterInformation: { labelOffsetSupport: true } } },
  },
  workspace: { configuration: true, workspaceFolders: true },
};

/** One running language server: JSON-RPC over the app's bridge, for the documents of one package folder. */
export class LanguageClient {
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private readonly handlers = new Map<string, Handler>();
  private readonly versions = new Map<string, number>();
  private closed = false;

  private constructor(
    readonly session: LspSession,
    readonly root: string,
    private readonly settings: Partial<Record<string, unknown>>,
  ) {}

  /** Starts the server and completes LSP's handshake. */
  static async start(server: LspServer, packageId: string): Promise<LanguageClient> {
    LanguageClient.listen();
    const started = await commands.lspStart(server, packageId);
    if (started.status === "error") throw new Error(describeLspError(started.error));
    const client = new LanguageClient(started.data.session, started.data.root, started.data.settings);
    LanguageClient.live.set(client.session, client);
    await client.request("initialize", {
      processId: null,
      rootUri: client.root,
      workspaceFolders: [{ uri: client.root, name: packageId }],
      capabilities: CLIENT_CAPABILITIES,
    });
    client.notify("initialized", {});
    return client;
  }

  /** The `file://` URI of `file` in this server's folder. */
  uriOf(file: string): string {
    return `${this.root}/${file}`;
  }

  onNotification(method: string, handler: Handler) {
    this.handlers.set(method, handler);
  }

  open(file: string, languageId: string, text: string) {
    const uri = this.uriOf(file);
    this.versions.set(uri, 1);
    this.notify("textDocument/didOpen", { textDocument: { uri, languageId, version: 1, text } });
  }

  /** The whole new text, which every server accepts (full document sync). */
  change(file: string, text: string) {
    const uri = this.uriOf(file);
    const version = (this.versions.get(uri) ?? 0) + 1;
    this.versions.set(uri, version);
    this.notify("textDocument/didChange", { textDocument: { uri, version }, contentChanges: [{ text }] });
  }

  request<T = unknown>(method: string, params: Json): Promise<T> {
    if (this.closed) return Promise.reject(new Error("the language server has stopped"));
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (result: unknown) => void, reject });
      this.send({ jsonrpc: "2.0", id, method, params });
    });
  }

  notify(method: string, params: Json) {
    if (!this.closed) this.send({ jsonrpc: "2.0", method, params });
  }

  async stop() {
    if (this.closed) return;
    this.closed = true;
    LanguageClient.live.delete(this.session);
    this.failPending("the language server was stopped");
    await commands.lspStop(this.session);
  }

  private send(message: Json) {
    void commands.lspSend(this.session, JSON.stringify(message)).then((sent) => {
      if (sent.status === "error") console.error("sending to a language server:", describeLspError(sent.error));
    });
  }

  private receive(message: Json) {
    const { id, method } = message as { id?: number | string; method?: string };
    if (method !== undefined && id !== undefined) return this.answerServerRequest(id, method, message.params as Json);
    if (method !== undefined) return this.handlers.get(method)?.(message.params as Json);
    if (typeof id !== "number") return;
    const pending = this.pending.get(id);
    this.pending.delete(id);
    const error = message.error as { message?: string } | undefined;
    if (error) pending?.reject(new Error(error.message ?? "the language server refused the request"));
    else pending?.resolve(message.result);
  }

  /** Requests the server makes of the editor: settings come from the app; the rest needs no answer. */
  private answerServerRequest(id: number | string, method: string, params: Json) {
    const sections = ((params.items as { section?: string }[] | undefined) ?? []).map((item) => item.section ?? "");
    const result = method === "workspace/configuration" ? sections.map((section) => this.settings[section] ?? null) : null;
    this.send({ jsonrpc: "2.0", id, result });
  }

  private exited(reason: string) {
    this.closed = true;
    LanguageClient.live.delete(this.session);
    this.failPending(`the language server stopped: ${reason}`);
  }

  private failPending(reason: string) {
    for (const { reject } of this.pending.values()) reject(new Error(reason));
    this.pending.clear();
  }

  private static readonly live = new Map<LspSession, LanguageClient>();
  private static listening = false;

  /** Routes every server's messages to its client, from the first start on. */
  private static listen() {
    if (LanguageClient.listening) return;
    LanguageClient.listening = true;
    void events.lspMessage.listen(({ payload }) => {
      const client = LanguageClient.live.get(payload.session);
      if (!client) return;
      try {
        client.receive(JSON.parse(payload.message) as Json);
      } catch (e) {
        console.error("a language server sent something that isn't JSON-RPC:", e);
      }
    });
    void events.lspExited.listen(({ payload }) => LanguageClient.live.get(payload.session)?.exited(payload.reason));
  }
}
