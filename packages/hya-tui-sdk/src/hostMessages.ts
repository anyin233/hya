/** Messages between the shared host's main thread (src/main.ts) and its VM worker (src/worker.ts). */

export type ToWorker =
  | { readonly type: "load"; readonly seq: number; readonly ext: string; readonly script: string }
  | { readonly type: "unload"; readonly seq: number; readonly ext: string }
  /** `request`: the extension JSON-RPC request frame, as JSON text. */
  | { readonly type: "call"; readonly seq: number; readonly ext: string; readonly id: string | number; readonly request: string }
  | { readonly type: "hostResult"; readonly call: number; readonly value?: unknown; readonly error?: string }

export type FromWorker =
  /** The end of a load, unload, or call; `response` is the extension's response frame as JSON text. */
  | { readonly type: "done"; readonly seq: number; readonly response?: string; readonly error?: string }
  | { readonly type: "host"; readonly call: number; readonly ext: string; readonly method: string; readonly params: Record<string, unknown> }
  | { readonly type: "log"; readonly ext: string; readonly line: string }
