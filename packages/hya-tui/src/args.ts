import { resolve } from "node:path"

export interface FrontendOptions {
  server: string
  directory: string
  grpc?: string
}

/** Select one backend transport and validate its listener address. */
export function argumentsFrom(argv: string[]): FrontendOptions | null {
  let server = "http://127.0.0.1:8080"
  let directory = process.cwd()
  let grpc: string | undefined
  let explicitHttp = false
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index]
    if (arg === "--help" || arg === "-h") return null
    if (arg === "--server" && argv[index + 1]) {
      server = argv[++index]!
      explicitHttp = true
    } else if (arg === "--grpc" && argv[index + 1]) {
      grpc = argv[++index]!
    } else if (arg === "--dir" && argv[index + 1]) {
      directory = argv[++index]!
    } else {
      throw new Error(`Unknown or incomplete option: ${arg}`)
    }
  }
  if (grpc) {
    if (explicitHttp) throw new Error("Use either --server or --grpc, not both")
    const address = grpc.replace(/^grpc:\/\//, "")
    const url = new URL(`http://${address}`)
    if (!url.hostname || !url.port || url.pathname !== "/" || url.search || url.hash || url.username || url.password) {
      throw new Error("--grpc needs a host:port without a path or credentials")
    }
    return { server: `grpc://${url.host}`, directory: resolve(directory), grpc: url.host }
  }
  const url = new URL(server)
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("--server needs an HTTP URL")
  return { server: url.toString(), directory: resolve(directory) }
}
