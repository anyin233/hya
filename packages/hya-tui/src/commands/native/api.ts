import { brief, operations } from "../../api"
import { parseApiCommand } from "../../client"
import { matchValues, type CommandSpec } from "../registry"

export const apiCommand: CommandSpec = {
  name: "/api",
  description: "List all v1 HTTP operations, or call one",
  argumentHint: "[METHOD /v1/path [JSON object]]",
  complete: ({ words, current, head }, context) => {
    if (words.length === 1) return matchValues(head, current, ["GET", "POST", "PUT", "PATCH", "DELETE"])
    if (words.length === 2) {
      const method = words[0]?.toUpperCase() ?? ""
      return context.apiOperations
        .filter((operation) => operation.startsWith(`${method} `))
        .map((operation) => operation.slice(method.length + 1))
        .filter((path) => path.toLowerCase().startsWith(current.toLowerCase()))
        .sort()
        .map((path) => `${head}${path}`)
    }
    return []
  },
  run: async ({ store, client }, { args, text }) => {
    store.setView("api")
    if (args.length > 0) {
      const request = parseApiCommand(text)
      const result = await client.request<unknown>(request.method, request.path, request.body)
      store.setApiOutput(`${request.method} ${request.path}\n\n${brief(result)}\n\n/api shows the operation catalog.`)
    } else store.setApiOutput(`Use /api METHOD /v1/path [JSON object].\n\n${operations()}`)
  },
}
