// Hand-rolled framed stdio LSP client for zio-lsp.
// No vscode-languageclient dependency. Only the vscode API and Node child_process.

import * as vscode from "vscode";
import { spawn, ChildProcessWithoutNullStreams } from "node:child_process";

type JsonRpcId = number | string;

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (reason: { code: number; message: string }) => void;
}

interface DiagnosticsParams {
  uri: string;
  version?: number;
  diagnostics: Diagnostic[];
}

interface LogMessageParams {
  type: 1 | 2 | 3 | 4;
  message: string;
}

interface ShowMessageParams {
  type: 1 | 2 | 3 | 4;
  message: string;
}

interface InitializeResult {
  capabilities: unknown;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasId(value: Record<string, unknown>): value is Record<string, unknown> & { id: JsonRpcId } {
  return typeof value.id === "number" || typeof value.id === "string";
}

function isResponse(value: Record<string, unknown>): value is Record<string, unknown> & { id: JsonRpcId; result?: unknown; error?: { code: number; message: string } } {
  return hasId(value) && ("result" in value || "error" in value);
}

function isNotification(value: Record<string, unknown>): value is Record<string, unknown> & { method: string; params?: unknown } {
  return typeof value.method === "string";
}

function isDiagnosticsParams(value: unknown): value is DiagnosticsParams {
  if (!isObject(value)) return false;
  if (typeof value.uri !== "string") return false;
  if (!Array.isArray(value.diagnostics)) return false;
  if ("version" in value && typeof value.version !== "number") return false;
  return true;
}

function isLogParams(value: unknown): value is LogMessageParams {
  if (!isObject(value)) return false;
  return typeof value.type === "number" && typeof value.message === "string";
}

function isShowMessageParams(value: unknown): value is ShowMessageParams {
  if (!isObject(value)) return false;
  return typeof value.type === "number" && typeof value.message === "string";
}

interface Diagnostic {
  range: { start: { line: number; character: number }; end: { line: number; character: number } };
  message: string;
  severity: number;
  source?: string;
}

class LspClient {
  private readonly child: ChildProcessWithoutNullStreams;
  private nextId = 1;
  private readonly pending = new Map<JsonRpcId, PendingRequest>();
  private buffer = Buffer.alloc(0);
  private capabilities: unknown = {};
  private readonly diagnosticsHandler: (uri: string, version: number | undefined, items: Diagnostic[]) => void;
  private readonly logHandler: (severity: 1 | 2 | 3 | 4, message: string) => void;

  constructor(
    command: string,
    args: string[],
    diagnosticsHandler: (uri: string, version: number | undefined, items: Diagnostic[]) => void,
    logHandler: (severity: 1 | 2 | 3 | 4, message: string) => void,
  ) {
    this.diagnosticsHandler = diagnosticsHandler;
    this.logHandler = logHandler;
    this.child = spawn(command, args, { stdio: ["pipe", "pipe", "pipe"] });
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk: string) => this.logHandler(1, `zio-lsp stderr: ${chunk}`));
    this.child.on("exit", (code, signal) => {
      this.logHandler(2, `zio-lsp exited code=${code} signal=${signal}`);
      const reason = { code: -32099, message: "zio-lsp exited" };
      for (const pending of this.pending.values()) pending.reject(reason);
      this.pending.clear();
    });
    this.child.stdout.on("data", (chunk: Buffer) => this.consume(chunk));
  }

  private consume(chunk: Buffer): void {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (true) {
      const headerEnd = this.buffer.indexOf("\r\n\r\n");
      if (headerEnd < 0) return;
      const header = this.buffer.subarray(0, headerEnd).toString("ascii");
      const match = /Content-Length:\s*(\d+)/i.exec(header);
      if (!match) {
        this.logHandler(1, `invalid LSP header: ${JSON.stringify(header)}`);
        return;
      }
      const length = Number(match[1]);
      const bodyStart = headerEnd + 4;
      if (this.buffer.length < bodyStart + length) return;
      const body = this.buffer.subarray(bodyStart, bodyStart + length).toString("utf8");
      this.buffer = this.buffer.subarray(bodyStart + length);
      let raw: unknown;
      try { raw = JSON.parse(body); }
      catch (err) {
        this.logHandler(1, `invalid LSP body: ${(err as Error).message}`);
        continue;
      }
      if (!isObject(raw)) {
        this.logHandler(1, "LSP message is not an object");
        continue;
      }
      if (isResponse(raw)) {
        const pending = this.pending.get(raw.id);
        if (!pending) continue;
        this.pending.delete(raw.id);
        if (raw.error) pending.reject(raw.error);
        else pending.resolve(raw.result);
        continue;
      }
      if (isNotification(raw)) {
        this.handleNotification(raw.method, raw.params);
      }
    }
  }

  private handleNotification(method: string, params: unknown): void {
    switch (method) {
      case "textDocument/publishDiagnostics":
        if (isDiagnosticsParams(params)) {
          this.diagnosticsHandler(params.uri, params.version, params.diagnostics ?? []);
        }
        return;
      case "window/logMessage":
        if (isLogParams(params)) this.logHandler(params.type ?? 1, params.message ?? "");
        return;
      case "window/showMessage":
        if (isShowMessageParams(params)) void vscode.window.showInformationMessage(params.message ?? "");
        return;
      default:
        this.logHandler(1, `unhandled server notification: ${method}`);
    }
  }

  private send(message: Record<string, unknown>): void {
    const body = Buffer.from(JSON.stringify(message), "utf8");
    const header = Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, "ascii");
    this.child.stdin.write(Buffer.concat([header, body]));
  }

  request<T>(method: string, params: unknown): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.send({ jsonrpc: "2.0", id, method, params });
    });
  }

  notify(method: string, params: unknown): void {
    this.send({ jsonrpc: "2.0", method, params });
  }

  rememberCapabilities(capabilities: unknown): void { this.capabilities = capabilities; }
  capabilitiesSnapshot(): unknown { return this.capabilities; }

  stop(): void {
    try { this.child.kill(); } catch { /* child already gone */ }
  }
}

interface DocumentState {
  version: number;
}

class ZioLspSession implements vscode.Disposable {
  private readonly client: LspClient;
  private readonly documents = new Map<string, DocumentState>();
  private readonly diagnostics = vscode.languages.createDiagnosticCollection("zio");
  private readonly disposables: vscode.Disposable[] = [];

  constructor(command: string, args: string[]) {
    this.client = new LspClient(
      command, args,
      (uri, version, items) => this.applyDiagnostics(uri, version, items),
      (severity, message) => this.log(severity, message),
    );

    this.disposables.push(
      vscode.workspace.onDidOpenTextDocument((doc) => this.didOpen(doc)),
      vscode.workspace.onDidChangeTextDocument((event) => this.didChange(event)),
      vscode.workspace.onDidCloseTextDocument((doc) => this.didClose(doc)),
      vscode.workspace.onDidSaveTextDocument((doc) => this.didSave(doc)),
    );

    for (const doc of vscode.workspace.textDocuments) {
      if (doc.languageId === "zio") this.didOpen(doc);
    }
  }

  async start(rootUri: string | null, initializationOptions: unknown): Promise<void> {
    const folders = vscode.workspace.workspaceFolders?.map((f) => ({ uri: f.uri.toString(), name: f.name })) ?? [];
    const params = {
      processId: process.pid,
      rootUri,
      workspaceFolders: folders,
      capabilities: {
        workspace: { workspaceFolders: true },
        textDocument: {
          synchronization: { didSave: true, willSave: false, dynamicRegistration: false },
          publishDiagnostics: { relatedInformation: false },
          documentSymbol: { hierarchical: false },
          definition: { linkSupport: false },
          references: {},
          hover: { contentFormat: ["plaintext", "markdown"] },
          completion: { completionItem: { snippetSupport: false } },
          semanticTokens: {
            tokenTypes: ["namespace", "type", "function", "variable", "keyword", "string", "number", "comment", "operator"],
            tokenModifiers: ["declaration"],
            formats: ["relative"],
            full: { delta: false },
          },
        },
      },
      initializationOptions,
    };
    const result = await this.client.request<InitializeResult>("initialize", params);
    this.client.rememberCapabilities(result.capabilities);
    this.client.notify("initialized", {});
  }

  private applyDiagnostics(uri: string, _version: number | undefined, items: Diagnostic[]): void {
    const vsUri = vscode.Uri.parse(uri);
    if (items.length === 0) {
      this.diagnostics.delete(vsUri);
      return;
    }
    const converted = items.map((d) => new vscode.Diagnostic(
      new vscode.Range(
        new vscode.Position(d.range.start.line, d.range.start.character),
        new vscode.Position(d.range.end.line, d.range.end.character),
      ),
      d.message,
      d.severity ?? vscode.DiagnosticSeverity.Error,
    ));
    this.diagnostics.set(vsUri, converted);
  }

  private didOpen(doc: vscode.TextDocument): void {
    if (doc.languageId !== "zio" || doc.uri.scheme !== "file") return;
    if (this.documents.has(doc.uri.toString())) return;
    const version = doc.version;
    this.documents.set(doc.uri.toString(), { version });
    this.client.notify("textDocument/didOpen", {
      textDocument: {
        uri: doc.uri.toString(),
        languageId: doc.languageId,
        version,
        text: doc.getText(),
      },
    });
  }

  private didChange(event: vscode.TextDocumentChangeEvent): void {
    const uri = event.document.uri.toString();
    if (!this.documents.has(uri)) return;
    const version = event.document.version;
    this.documents.set(uri, { version });
    const changes = event.contentChanges.map((change) => ({
      range: {
        start: { line: change.range.start.line, character: change.range.start.character },
        end: { line: change.range.end.line, character: change.range.end.character },
      },
      rangeLength: change.rangeLength,
      text: change.text,
    }));
    this.client.notify("textDocument/didChange", {
      textDocument: { uri, version },
      contentChanges: changes,
    });
  }

  private didClose(doc: vscode.TextDocument): void {
    const uri = doc.uri.toString();
    if (!this.documents.delete(uri)) return;
    this.diagnostics.delete(doc.uri);
    this.client.notify("textDocument/didClose", { textDocument: { uri } });
  }

  private didSave(doc: vscode.TextDocument): void {
    const uri = doc.uri.toString();
    if (!this.documents.has(uri)) return;
    this.client.notify("textDocument/didSave", { textDocument: { uri } });
  }

  capabilities(): unknown { return this.client.capabilitiesSnapshot(); }

  request<T>(method: string, params: unknown): Promise<T> { return this.client.request<T>(method, params); }

  async shutdown(): Promise<void> {
    try { await this.client.request<unknown>("shutdown", null); } catch { /* server may have died */ }
    this.client.notify("exit", null);
    this.client.stop();
  }

  private log(severity: 1 | 2 | 3 | 4, message: string): void {
    if (severity === 1) console.error("[zio-lsp]", message);
    else console.log("[zio-lsp]", message);
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
    this.diagnostics.dispose();
    this.client.stop();
    this.documents.clear();
  }
}

let session: ZioLspSession | undefined;

function resolveServer(): { command: string; args: string[] } {
  const config = vscode.workspace.getConfiguration("zio");
  const configured = (config.get<string>("serverPath") ?? "zio-lsp").trim();
  const args = (config.get<string[]>("serverArgs") ?? []).map((s) => s.trim()).filter((s) => s.length > 0);
  return { command: configured, args };
}

function uriRange(start: { line: number; character: number }, end: { line: number; character: number }): vscode.Range {
  return new vscode.Range(new vscode.Position(start.line, start.character), new vscode.Position(end.line, end.character));
}

function uriLocation(loc: { uri: string; range: { start: { line: number; character: number }; end: { line: number; character: number } } }): vscode.Location {
  return new vscode.Location(vscode.Uri.parse(loc.uri), uriRange(loc.range.start, loc.range.end));
}

function registerProviders(session: ZioLspSession): vscode.Disposable[] {
  const disposable: vscode.Disposable[] = [];

  disposable.push(vscode.languages.registerDocumentSymbolProvider({ language: "zio" }, {
    provideDocumentSymbols: async (doc) => {
      const uri = doc.uri.toString();
      const result = await session.request<Array<{ name: string; kind: number; detail?: string; range: { start: { line: number; character: number }; end: { line: number; character: number } } }>>("textDocument/documentSymbol", { textDocument: { uri } });
      return result.map((s) => new vscode.SymbolInformation(
        s.name,
        // LSP wire-format SymbolKind → vscode enum (stable integer mapping)
        s.kind as unknown as vscode.SymbolKind,
        s.detail ?? "",
        new vscode.Location(vscode.Uri.parse(uri), uriRange(s.range.start, s.range.end)),
      ));
    },
  }));

  disposable.push(vscode.languages.registerDefinitionProvider({ language: "zio" }, {
    provideDefinition: async (doc, position) => {
      const uri = doc.uri.toString();
      const result = await session.request<Array<{ uri: string; range: { start: { line: number; character: number }; end: { line: number; character: number } } }>>("textDocument/definition", { textDocument: { uri }, position: { line: position.line, character: position.character } });
      return result.map(uriLocation);
    },
  }));

  disposable.push(vscode.languages.registerReferenceProvider({ language: "zio" }, {
    provideReferences: async (doc, position, context) => {
      const uri = doc.uri.toString();
      const result = await session.request<Array<{ uri: string; range: { start: { line: number; character: number }; end: { line: number; character: number } } }>>("textDocument/references", { textDocument: { uri }, position: { line: position.line, character: position.character }, context: { includeDeclaration: context.includeDeclaration } });
      return result.map(uriLocation);
    },
  }));

  disposable.push(vscode.languages.registerHoverProvider({ language: "zio" }, {
    provideHover: async (doc, position) => {
      const uri = doc.uri.toString();
      const result = await session.request<{ contents: { kind?: string; value: string } | string; range?: { start: { line: number; character: number }; end: { line: number; character: number } } } | null>("textDocument/hover", { textDocument: { uri }, position: { line: position.line, character: position.character } });
      if (!result) return undefined;
      const value = typeof result.contents === "string" ? result.contents : result.contents.value;
      const range = result.range ? uriRange(result.range.start, result.range.end) : new vscode.Range(position, position);
      return new vscode.Hover(new vscode.MarkdownString(value), range);
    },
  }));

  disposable.push(vscode.languages.registerCompletionItemProvider({ language: "zio" }, {
    provideCompletionItems: async (doc, position) => {
      const uri = doc.uri.toString();
      const result = await session.request<{ items: Array<{ label: string; kind?: number; detail?: string }> }>("textDocument/completion", { textDocument: { uri }, position: { line: position.line, character: position.character } });
      return result.items.map((item) => {
        // LSP wire-format CompletionItemKind → vscode enum (stable integer mapping)
        const ci = new vscode.CompletionItem(item.label, item.kind as unknown as vscode.CompletionItemKind);
        if (item.detail) ci.documentation = new vscode.MarkdownString(item.detail);
        return ci;
      });
    },
  }, "/", ":"));

  disposable.push(
    vscode.languages.registerDocumentSemanticTokensProvider({ language: "zio" }, {
      provideDocumentSemanticTokens: async (doc) => {
        const uri = doc.uri.toString();
        const result = await session.request<{ data: number[] }>("textDocument/semanticTokens/full", { textDocument: { uri } });
        const builder = new vscode.SemanticTokensBuilder();
        const data = result.data;
        let prevLine = 0; let prevStartChar = 0;
        for (let i = 0; i + 4 < data.length; i += 5) {
          const lineDelta = data[i];
          const startCharDelta = data[i + 1];
          const length = data[i + 2];
          const tokenType = data[i + 3];
          const tokenModifiers = data[i + 4];
          const line = prevLine + lineDelta;
          const startChar = lineDelta === 0 ? prevStartChar + startCharDelta : startCharDelta;
          builder.push(line, startChar, length, tokenType, tokenModifiers);
          prevLine = line;
          prevStartChar = startChar;
        }
        return builder.build();
      },
    }, new vscode.SemanticTokensLegend(
      ["namespace", "type", "function", "variable", "keyword", "string", "number", "comment", "operator"],
      ["declaration"],
    )),
  );

  return disposable;
}

export function activate(context: vscode.ExtensionContext): void {
  void vscode.window.createOutputChannel("Zio");
  const { command, args } = resolveServer();
  session = new ZioLspSession(command, args);
  context.subscriptions.push(session);

  const workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.toString() ?? null;
  void session.start(workspaceRoot, { moduleRoots: [] }).then(
    () => {
      if (!session) return;
      const providers = registerProviders(session);
      for (const d of providers) context.subscriptions.push(d);
    },
    (err: unknown) => {
      void vscode.window.showErrorMessage(`Failed to start zio-lsp: ${(err as Error).message ?? String(err)}`);
    },
  );
}

export function deactivate(): Thenable<void> {
  if (!session) return Promise.resolve();
  const active = session;
  session = undefined;
  return active.shutdown();
}
