import { JSONRPC_VERSION, MAX_FRAME_BYTES, type HostCommand, type JsonValue, type RpcEnvelope, type RpcId, type TuiFrame } from "./wire";

export {
  JSONRPC_VERSION, MAX_FRAME_BYTES, REPLACEABLE_PANES, REPLACEABLE_SURFACES,
  TUI_EXTENSION_API_VERSION, TUI_EXTENSION_PERMISSIONS, TUI_EXTENSION_SDK_VERSION, VM_STOPPED_ERROR_CODE,
  type CommandResult, type Contributions, type ExtensionAction, type ExtensionContext, type FormatInput,
  type HostCommand, type InterceptDecision, type JsonValue, type KeyEvent, type PanelPlacement,
  type ReplaceablePane, type ReplaceableSurface, type RpcEnvelope, type RpcError, type RpcId,
  type SurfaceRef, type ToolCallInput, type TuiExtensionPermission, type TuiFrame, type TuiMethod,
  type TuiMethodMap, type TuiRequest, type TuiResponse,
} from "./wire";

/** Longest an extension may execute for one request (waiting for `api.fs` does not count). */
export const VM_DEADLINE_MS = 2_000;

/** Strings show a notice; void and objects invalidate by default. */
export type HandlerResult = string | void | { readonly notice?: string; readonly commands?: readonly HostCommand[]; readonly invalidate?: boolean };

export type ParseFrameResult =
  | { readonly ok: true; readonly frame: RpcEnvelope }
  | { readonly ok: false; readonly message: string };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isId(value: unknown): value is RpcId {
  return typeof value === "string" || (typeof value === "number" && Number.isSafeInteger(value));
}

function isJson(value: unknown): value is JsonValue {
  if (value === null || typeof value === "string" || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value) && (!Number.isInteger(value) || Number.isSafeInteger(value));
  if (Array.isArray(value)) return value.every(isJson);
  return isRecord(value) && Object.values(value).every(isJson);
}

/** Decode one LF-delimited JSON-RPC frame (no batches or notifications in v1). */
export function parseFrame(line: string): ParseFrameResult {
  if (new TextEncoder().encode(line).byteLength > MAX_FRAME_BYTES) {
    return { ok: false, message: `frame exceeds ${MAX_FRAME_BYTES} bytes` };
  }
  try {
    const frame: unknown = JSON.parse(line);
    if (!isRecord(frame) || frame.jsonrpc !== JSONRPC_VERSION || !isJson(frame)) {
      return { ok: false, message: "expected a JSON-safe JSON-RPC 2.0 object" };
    }
    const keys = Object.keys(frame);
    if (typeof frame.method === "string" && frame.method.length > 0 && isId(frame.id)
      && "params" in frame && keys.every(key => ["jsonrpc", "id", "method", "params"].includes(key))) {
      return { ok: true, frame: frame as RpcEnvelope };
    }
    if (isId(frame.id) && "result" in frame && keys.every(key => ["jsonrpc", "id", "result"].includes(key))) {
      return { ok: true, frame: frame as RpcEnvelope };
    }
    if ((frame.id === null || isId(frame.id)) && isRecord(frame.error)
      && typeof frame.error.code === "number" && Number.isSafeInteger(frame.error.code)
      && typeof frame.error.message === "string"
      && Object.keys(frame.error).every(key => ["code", "message", "data"].includes(key))
      && keys.every(key => ["jsonrpc", "id", "error"].includes(key))) {
      return { ok: true, frame: frame as RpcEnvelope };
    }
    return { ok: false, message: "invalid request or response envelope" };
  } catch {
    return { ok: false, message: "invalid JSON frame" };
  }
}

/** Encode exactly one line; reject values JSON.stringify would silently lose. */
export function encodeFrame(frame: TuiFrame | RpcEnvelope): string {
  if (!isJson(frame)) throw new TypeError("frame must contain only JSON-safe values");
  const json = JSON.stringify(frame);
  const parsed = parseFrame(json);
  if (!parsed.ok) throw new TypeError(parsed.message);
  return `${json}\n`;
}
