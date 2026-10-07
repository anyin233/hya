import { createInterface } from "node:readline";
import { dispatch } from "./provider";

/** Stdout is exclusively NDJSON JSON-RPC; diagnostics belong on stderr. */
export async function serve(): Promise<void> {
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  try {
    for await (const line of input) {
      let message: unknown;
      try {
        message = JSON.parse(line);
      } catch {
        await write({ jsonrpc: "2.0", id: null, error: { code: -32700, message: "Parse error" } });
        continue;
      }
      const result = dispatch(message);
      if (result.frame !== undefined) await write(result.frame);
      if (result.shutdown) break;
    }
  } finally {
    input.close();
  }
}

function write(frame: unknown): Promise<void> {
  return new Promise((resolve, reject) => {
    process.stdout.write(`${JSON.stringify(frame)}\n`, (error) => error ? reject(error) : resolve());
  });
}

if (import.meta.main) {
  serve().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
