import {
  BoxRenderable,
  InputRenderable,
  InputRenderableEvents,
  CliRenderEvents,
  ScrollBoxRenderable,
  TextRenderable,
  createCliRenderer,
  type KeyEvent,
} from "@opentui/core"
import type { PasteEvent } from "@opentui/core"
import { resolve } from "node:path"
import openapi from "../../../docs/protocol/openapi.json"
import { completeCommand, SecretEntry, type CompletionContext } from "./completion"
import { footerInstruction, type View } from "./instructions"
import {
  HyaClient,
  parseApiCommand,
  type AgentSummary,
  type CommandSummary,
  type Interaction,
  type MessageInfo,
  type ModelSummary,
  type ProviderSummary,
  type SessionInfo,
  type StreamFrame,
  type WorkflowSummary,
} from "./client"

function argumentsFrom(argv: string[]): { server: string; directory: string } | null {
  let server = "http://127.0.0.1:8080"
  let directory = process.cwd()
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index]
    if (arg === "--help" || arg === "-h") return null
    if (arg === "--server" && argv[index + 1]) server = argv[++index]!
    else if (arg === "--dir" && argv[index + 1]) directory = argv[++index]!
    else throw new Error(`Unknown or incomplete option: ${arg}`)
  }
  const url = new URL(server)
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("--server needs an HTTP URL")
  return { server: url.toString(), directory: resolve(directory) }
}

function modelReference(session: SessionInfo): string {
  const model = session.model
  return model?.providerId && model.modelId ? `${model.providerId}/${model.modelId}` : ""
}

function formatMessage(message: MessageInfo): string {
  const role = message.role.replace(/^ROLE_/, "").toLowerCase()
  const lines = (message.parts ?? []).map((part) => {
    if (part.text) return part.text.text
    if (part.toolCall) return `↳ ${part.toolCall.tool}  ${part.toolCall.state ?? ""}`
    if (part.toolResult) return `  ${part.toolResult.errorMessage ?? part.toolResult.output}`
    if (part.attachment) return `  attachment: ${part.attachment.name}`
    return ""
  }).filter(Boolean)
  return `${role}${message.finish ? ` · ${message.finish.replace(/^FINISH_REASON_/, "").toLowerCase()}` : ""}\n${lines.join("\n") || "…"}`
}

function operations(): string {
  const paths = openapi.paths as Record<string, Record<string, { operationId?: string; "x-server-streaming"?: boolean }>>
  return Object.entries(paths).flatMap(([path, methods]) =>
    Object.entries(methods).map(([method, detail]) =>
      `${method.toUpperCase().padEnd(6)} ${path}  ${detail.operationId ?? ""}${detail["x-server-streaming"] ? " [stream]" : ""}`,
    ),
  ).sort().join("\n")
}

const apiOperationNames = Object.entries(openapi.paths as Record<string, Record<string, unknown>>)
  .flatMap(([path, methods]) => Object.keys(methods).map((method) => `${method.toUpperCase()} ${path}`))

function brief(value: unknown): string {
  const text = JSON.stringify(value, null, 2) ?? "null"
  return text.length > 20_000 ? `${text.slice(0, 20_000)}\n… output truncated` : text
}

async function main(): Promise<void> {
  const parsedOptions = argumentsFrom(process.argv.slice(2))
  if (!parsedOptions) {
    process.stdout.write("Usage: bun packages/hya-tui/src/main.ts [--server http://127.0.0.1:8080] [--dir PATH]\n")
    return
  }
  const options = parsedOptions
  const client = new HyaClient(options.server, options.directory)
  const renderer = await createCliRenderer({ exitOnCtrlC: true, targetFps: 30 })
  const colors = { bg: "#11151b", panel: "#1c2530", fg: "#e8edf3", muted: "#9caab9", accent: "#73c8e8", border: "#405366" }
  const root = new BoxRenderable(renderer, { width: "100%", height: "100%", flexDirection: "column", backgroundColor: colors.bg })
  const header = new TextRenderable(renderer, { content: "hya · connecting…", height: 1, fg: colors.accent })
  const body = new BoxRenderable(renderer, { width: "100%", flexGrow: 1, flexDirection: "row" })
  const sessionPanel = new BoxRenderable(renderer, { width: 27, flexShrink: 1, border: true, borderColor: colors.border, title: "Sessions", backgroundColor: colors.panel, flexDirection: "column" })
  const sessionScroll = new ScrollBoxRenderable(renderer, { width: "100%", flexGrow: 1 })
  const sessionText = new TextRenderable(renderer, { content: "Loading…", width: "100%", wrapMode: "word", fg: colors.fg })
  sessionScroll.add(sessionText)
  sessionPanel.add(sessionScroll)
  const mainPanel = new BoxRenderable(renderer, { flexGrow: 1, flexBasis: 0, border: true, borderColor: colors.border, title: "Chat", backgroundColor: colors.bg, flexDirection: "column" })
  const mainScroll = new ScrollBoxRenderable(renderer, { width: "100%", flexGrow: 1, stickyScroll: true, stickyStart: "bottom" })
  const mainText = new TextRenderable(renderer, { content: "", width: "100%", wrapMode: "word", fg: colors.fg })
  mainScroll.add(mainText)
  mainPanel.add(mainScroll)
  const interactionPanel = new BoxRenderable(renderer, { width: 28, flexShrink: 1, border: true, borderColor: colors.border, title: "Pending", backgroundColor: colors.panel, flexDirection: "column" })
  const interactionScroll = new ScrollBoxRenderable(renderer, { width: "100%", flexGrow: 1 })
  const interactionText = new TextRenderable(renderer, { content: "", width: "100%", wrapMode: "word", fg: colors.fg })
  interactionScroll.add(interactionText)
  interactionPanel.add(interactionScroll)
  body.add(sessionPanel)
  body.add(mainPanel)
  body.add(interactionPanel)
  const status = new TextRenderable(renderer, { content: "Enter prompt · /help commands · Ctrl+R refresh · Ctrl+C quit", height: 1, fg: colors.muted })
  const inputPanel = new BoxRenderable(renderer, { height: 3, border: true, borderColor: colors.border, backgroundColor: colors.panel, paddingX: 1 })
  const input = new InputRenderable(renderer, { width: "100%", maxLength: 10_000, placeholder: "Message or /command", textColor: colors.fg, cursorColor: colors.accent })
  const secretText = new TextRenderable(renderer, { content: "", width: "100%", fg: colors.accent, visible: false })
  const footer = new TextRenderable(renderer, { content: footerInstruction("chat", true), width: "100%", height: 1, fg: colors.muted })
  inputPanel.add(input)
  inputPanel.add(secretText)
  root.add(header)
  root.add(body)
  root.add(status)
  root.add(inputPanel)
  root.add(footer)
  renderer.root.add(root)
  const adaptLayout = (): void => {
    sessionPanel.visible = renderer.width >= 58
    interactionPanel.visible = renderer.width >= 105
  }
  adaptLayout()
  renderer.on(CliRenderEvents.RESIZE, adaptLayout)
  input.focus()

  let sessions: SessionInfo[] = []
  let messages: MessageInfo[] = []
  let interactions: Interaction[] = []
  let agents: AgentSummary[] = []
  let models: ModelSummary[] = []
  let providers: ProviderSummary[] = []
  let savedKeys: string[] = []
  let savedKeysAvailable = true
  let backendCommands: CommandSummary[] = []
  let workflows: WorkflowSummary[] = []
  let workflowState: Record<string, unknown> | undefined
  let selected: SessionInfo | undefined
  let turnId = ""
  let view: View = "chat"
  let apiOutput = "Use /api METHOD /v1/path [JSON object] to call any HTTP/JSON endpoint.\n\n" + operations()
  let cursor = "0"
  let streamAbort: AbortController | undefined
  let refreshTimer: ReturnType<typeof setTimeout> | undefined
  let closing = false
  let secretProvider: string | undefined
  const secretEntry = new SecretEntry()

  function completionContext(): CompletionContext {
    return {
      backendCommands: backendCommands.map((command) => command.name),
      providers: [...new Set([...providers.map((provider) => provider.id), ...savedKeys])],
      savedKeys,
      models: models.map((model) => model.id),
      sessions: sessions.map((session) => session.id),
      workflows: workflows.map((workflow) => workflow.name),
      interactions: interactions.map((interaction) => interaction.id),
      agents: agents.map((agent) => agent.name),
      apiOperations: apiOperationNames,
    }
  }

  function finishSecretEntry(): void {
    secretProvider = undefined
    secretEntry.clear()
    secretText.content = ""
    secretText.visible = false
    input.visible = true
    input.focus()
    footer.content = footerInstruction(view, savedKeysAvailable)
  }

  function showStatus(text: string): void { status.content = text }
  function repaint(): void {
    footer.content = footerInstruction(view, savedKeysAvailable, secretProvider !== undefined)
    header.content = `hya ${selected ? `· ${selected.title || selected.id} · ${selected.agent} ${modelReference(selected)}` : "· no session"} · ${options.server}`
    sessionText.content = sessions.length
      ? sessions.map((session, index) => `${session.id === selected?.id ? "▸" : " "} ${index + 1}. ${session.title || session.id}\n   ${session.agent}${session.busy ? " · running" : ""}`).join("\n\n")
      : "No sessions. Type a prompt or /new."
    interactionText.content = interactions.length
      ? interactions.map((item) => `${item.type?.includes("QUESTION") ? "?" : "!"} ${item.title}\n${item.id}`).join("\n\n")
      : "No pending requests"
    mainPanel.title = ({ chat: "Chat", models: "Models", workflows: "Workflows", interactions: "Interactions", keys: "Saved provider keys", api: "API commands", help: "Help" } as const)[view]
    switch (view) {
      case "chat":
        mainText.content = messages.length ? messages.slice(-50).map(formatMessage).join("\n\n") : "No messages yet. Type a prompt below."
        mainScroll.scrollTop = mainScroll.scrollHeight
        break
      case "models":
        mainText.content = models.length ? models.map((model) => `${model.id}  ${model.displayName ?? ""}  ${model.auth ?? ""}`).join("\n") : "No models returned by server."
        break
      case "workflows":
        mainText.content = `${workflowState ? `Current: ${String(workflowState.workflow ?? "none")} · ${String(workflowState.status ?? "") }\n\n` : ""}${workflows.length ? workflows.map((workflow) => `${workflow.name} · ${workflow.stageCount ?? 0} stages\n${workflow.description ?? ""}`).join("\n\n") : "No workflows discovered."}`
        break
      case "interactions":
        mainText.content = interactions.length ? interactions.map((item) => `${item.type} · ${item.id}\n${item.title}\n${item.detail ?? ""}\n${(item.options ?? []).join(" | ")}`).join("\n\n") : "No pending interactions."
        break
      case "keys": {
        const ids = [...new Set([...providers.map((provider) => provider.id), ...savedKeys])].sort()
        mainText.content = !savedKeysAvailable
          ? "Key listing is unavailable on this backend. Restart with hya 0.41.0 or newer."
          : ids.length
          ? ids.map((id) => `${savedKeys.includes(id) ? "● saved" : "○ no saved key"}  ${id}`).join("\n")
          : "No providers or saved keys. Use /key set <provider> to add one."
        break
      }
      case "api": mainText.content = apiOutput; break
      case "help": mainText.content = helpText; break
    }
  }

  async function refresh(): Promise<void> {
    const [sessionRows, interactionRows, modelRows, workflowRows, providerRows, keyRows, commandRows] = await Promise.all([
      client.listSessions(), client.listInteractions(), client.listModels(), client.listWorkflows(),
      client.listProviders(), client.listSavedKeys(), client.listCommands(),
    ])
    sessions = sessionRows
    interactions = interactionRows
    models = modelRows
    workflows = workflowRows
    providers = providerRows
    savedKeysAvailable = keyRows !== null
    savedKeys = keyRows ?? []
    backendCommands = commandRows
    if (selected) selected = sessions.find((row) => row.id === selected?.id) ?? selected
    repaint()
  }

  async function refreshMessages(): Promise<void> {
    if (!selected) return
    const sessionId = selected.id
    const rows = await client.listMessages(sessionId)
    if (selected?.id !== sessionId) return
    messages = rows
    repaint()
  }

  function scheduleRefresh(): void {
    if (refreshTimer) clearTimeout(refreshTimer)
    refreshTimer = setTimeout(() => {
      void Promise.all([refreshMessages(), client.listInteractions().then((rows) => { interactions = rows; repaint() })])
        .catch((error: unknown) => showStatus(`Refresh failed: ${String(error)}`))
    }, 120)
  }

  async function onFrame(frame: StreamFrame): Promise<void> {
    if (frame.resync && selected) {
      const replay = await client.listEvents(selected.id, cursor)
      cursor = replay.nextSeq ?? cursor
      scheduleRefresh()
      return
    }
    const event = frame.event
    if (!event) return
    if (event.seq && BigInt(event.seq) > BigInt(cursor)) cursor = event.seq
    if (event.messageFinished && event.messageFinished.message === turnId) {
      turnId = ""
      showStatus(`Turn finished · ${event.messageFinished.finish ?? "done"}`)
    }
    scheduleRefresh()
  }

  function startStream(sessionId: string): void {
    streamAbort?.abort()
    const controller = new AbortController()
    streamAbort = controller
    void (async () => {
      while (!closing && !controller.signal.aborted) {
        try {
          await client.streamSession(sessionId, cursor, onFrame, controller.signal)
        } catch (error) {
          if (!controller.signal.aborted) showStatus(`Stream reconnecting: ${String(error)}`)
        }
        if (!controller.signal.aborted) await Bun.sleep(800)
      }
    })()
  }

  async function openSession(sessionId: string): Promise<void> {
    const session = sessions.find((row) => row.id === sessionId)
      ?? await client.request<SessionInfo>("GET", `/v1/sessions/${encodeURIComponent(sessionId)}`)
    selected = session
    view = "chat"
    cursor = session.lastSeq ?? "0"
    messages = []
    repaint()
    await refreshMessages()
    startStream(session.id)
  }

  async function newSession(agentArg?: string, modelArg?: string): Promise<void> {
    const agent = agentArg ?? agents.find((item) => !item.hidden)?.name ?? "build"
    const preferred = agents.find((item) => item.name === agent)?.model
    const model = modelArg ?? (preferred?.providerId && preferred.modelId ? `${preferred.providerId}/${preferred.modelId}` : models[0]?.id)
    if (!model) throw new Error("No model is available; configure a provider on the backend")
    const session = await client.createSession(agent, model, options.directory)
    await refresh()
    await openSession(session.id)
    showStatus(`Created ${session.id}`)
  }

  async function submit(value: string): Promise<void> {
    const text = value.trim()
    if (!text) return
    try {
      if (!text.startsWith("/")) {
        if (!selected) await newSession()
        if (!selected) throw new Error("Session creation failed")
        const turn = await client.createTurn(selected.id, text)
        turnId = turn.id
        showStatus(`Turn ${turn.state.toLowerCase()} · ${turn.id}`)
        scheduleRefresh()
        return
      }
      const [command, ...args] = text.split(/\s+/)
      switch (command) {
        case "/help": view = "help"; repaint(); break
        case "/sessions": view = "chat"; await refresh(); showStatus("Use /open <id> or /open <number>"); break
        case "/new": await newSession(args[0], args[1]); break
        case "/open": {
          const target = args[0]
          const id = target && /^\d+$/.test(target) ? sessions[Number(target) - 1]?.id : target
          if (!id) throw new Error("Usage: /open <session id or number>")
          await openSession(id)
          break
        }
        case "/models": view = "models"; await refresh(); break
        case "/keys": view = "keys"; await refresh(); break
        case "/login":
        case "/key": {
          const action = command === "/login" ? "set" : args[0]
          const provider = command === "/login" ? args[0] : args[1]
          if (!provider || !["set", "remove"].includes(action ?? "")) {
            throw new Error("Usage: /key set|remove <provider> or /login <provider>")
          }
          if (action === "remove") {
            await client.removeProviderKey(provider)
            view = "keys"
            await refresh()
            showStatus(`Removed key for ${provider} · restart backend to apply`)
          } else {
            secretProvider = provider
            secretEntry.clear()
            input.blur()
            input.visible = false
            secretText.visible = true
            secretText.content = "Key: "
            footer.content = footerInstruction(view, savedKeysAvailable, true)
            showStatus(`Enter API key for ${provider} · Enter saves · Esc cancels`)
          }
          break
        }
        case "/workflows":
          view = "workflows"
          await refresh()
          workflowState = selected ? await client.getWorkflowState(selected.id) : undefined
          repaint()
          break
        case "/interactions": view = "interactions"; await refresh(); break
        case "/refresh": await refresh(); await refreshMessages(); break
        case "/model": {
          if (!selected || !args[0]) throw new Error("Usage: /model <provider/model> in a session")
          selected = await client.updateSessionModel(selected.id, args[0])
          await refresh()
          break
        }
        case "/cancel": {
          if (!selected || !turnId) throw new Error("No active turn")
          await client.cancelTurn(selected.id, turnId)
          showStatus("Cancellation requested")
          break
        }
        case "/approve":
        case "/deny": {
          if (!args[0]) throw new Error(`Usage: ${command} <interaction id>`)
          await client.respondPermission(args[0], command === "/approve")
          interactions = await client.listInteractions()
          repaint()
          break
        }
        case "/answer": {
          if (!args[0] || args.length < 2) throw new Error("Usage: /answer <interaction id> <text>")
          await client.respondQuestion(args[0], args.slice(1).join(" "))
          interactions = await client.listInteractions()
          repaint()
          break
        }
        case "/workflow": {
          if (!selected || !["select", "run"].includes(args[0] ?? "")) throw new Error("Usage: /workflow select|run [name] in a session")
          const action = args[0]!
          const name = args[1] ?? ""
          if (action === "select" && !name) throw new Error("Usage: /workflow select <name>")
          await client.submitWorkflow(selected.id, action === "select" ? { select: { name } } : { run: { name } })
          workflowState = await client.getWorkflowState(selected.id)
          view = "workflows"
          showStatus(`Workflow ${action} submitted`)
          repaint()
          break
        }
        case "/api": {
          view = "api"
          if (args.length > 0) {
            const request = parseApiCommand(text)
            const result = await client.request<unknown>(request.method, request.path, request.body)
            apiOutput = `${request.method} ${request.path}\n\n${brief(result)}\n\n/api shows the operation catalog.`
          } else apiOutput = `Use /api METHOD /v1/path [JSON object].\n\n${operations()}`
          repaint()
          break
        }
        default: {
          if (!selected) await newSession()
          if (!selected) throw new Error("Session creation failed")
          const name = command?.slice(1) ?? ""
          const argumentsText = text.slice(command.length).trimStart()
          const turn = await client.createCommandTurn(selected.id, name, argumentsText)
          turnId = turn.id
          showStatus(turn.id ? `Command ${name} · ${turn.state.toLowerCase()}` : `Command ${name} finished`)
          scheduleRefresh()
        }
      }
    } catch (error) {
      showStatus(`Error: ${String(error)}`)
    }
  }

  const helpText = [
    "Type a prompt to create a session (if needed) and start a turn.",
    "Other /commands are sent to the backend command catalog.",
    "", "/new [agent] [model]   Create a session", "/sessions             Refresh session list", "/open <id|number>     Open a session",
    "/models               List models", "/model <provider/model> Change current session model", "/workflows            List workflows",
    "/keys                 List saved provider key names", "/key set <provider>   Enter a key in a concealed prompt",
    "/login <provider>     Alias for /key set", "/key remove <provider> Delete a saved key",
    "/workflow select <name> | /workflow run [name]", "/interactions         Show pending permissions and questions",
    "/approve <id> | /deny <id> | /answer <id> <text>", "/cancel               Cancel current turn", "/refresh              Refresh all views",
    "/api                  List all v1 HTTP operations", "/api METHOD /v1/path [JSON object]", "", "Tab completes commands · Ctrl+R refresh · Ctrl+C quit",
  ].join("\n")

  input.on(InputRenderableEvents.ENTER, (value: string) => {
    input.value = ""
    void submit(value)
  })
  input.on(InputRenderableEvents.INPUT, (value: string) => {
    if (!value.startsWith("/")) return
    const choices = completeCommand(value, completionContext())
    if (choices.length) showStatus(`Tab: ${choices.slice(0, 5).join("  ")}${choices.length > 5 ? "  …" : ""}`)
  })
  let completionChoices: string[] = []
  let completionIndex = -1
  let completionCurrent = ""
  const onKey = (key: KeyEvent): void => {
    if (secretProvider) {
      key.preventDefault()
      key.stopPropagation()
      if (key.name === "escape" || key.name === "esc") {
        finishSecretEntry()
        showStatus("Key entry cancelled")
      } else if (key.name === "backspace") {
        secretEntry.backspace()
        secretText.content = `Key: ${secretEntry.mask}`
      } else if (key.name === "return" || key.name === "enter" || key.sequence === "\r") {
        const provider = secretProvider
        const value = secretEntry.take()
        if (!value) {
          showStatus("API key cannot be empty · Esc cancels")
          return
        }
        finishSecretEntry()
        void client.setProviderKey(provider, value)
          .then(async () => {
            view = "keys"
            await refresh()
            showStatus(`Saved key for ${provider} · restart backend to apply`)
          })
          .catch((error: unknown) => showStatus(`Key save failed: ${String(error)}`))
      } else if (!key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " ") {
        secretEntry.append(key.sequence)
        secretText.content = `Key: ${secretEntry.mask}`
      }
      return
    }
    if (key.name === "tab" || key.sequence === "\t") {
      key.preventDefault()
      key.stopPropagation()
      if (completionCurrent !== input.value) {
        completionChoices = completeCommand(input.value, completionContext())
        completionIndex = -1
      }
      if (completionChoices.length) {
        completionIndex = (completionIndex + 1) % completionChoices.length
        input.value = completionChoices[completionIndex] ?? input.value
        completionCurrent = input.value
        showStatus(`${completionIndex + 1}/${completionChoices.length} completion · Tab cycles`)
      }
      return
    }
    if (key.ctrl && key.name === "r") {
      void refresh().then(refreshMessages).catch((error: unknown) => showStatus(`Refresh failed: ${String(error)}`))
    }
  }
  const onPaste = (event: PasteEvent): void => {
    if (!secretProvider) return
    event.preventDefault()
    event.stopPropagation()
    secretEntry.append(new TextDecoder().decode(event.bytes))
    secretText.content = `Key: ${secretEntry.mask}`
  }
  renderer.keyInput.on("keypress", onKey)
  renderer.keyInput.on("paste", onPaste)
  renderer.once("destroy", () => {
    closing = true
    streamAbort?.abort()
    if (refreshTimer) clearTimeout(refreshTimer)
    renderer.keyInput.off("keypress", onKey)
    renderer.keyInput.off("paste", onPaste)
    renderer.off(CliRenderEvents.RESIZE, adaptLayout)
  })

  try {
    const bootstrap = await client.bootstrap()
    agents = bootstrap.agents ?? []
    models = bootstrap.models ?? []
    interactions = bootstrap.interactions ?? []
    await refresh()
    if (sessions[0]) await openSession(sessions[0].id)
    showStatus(savedKeysAvailable
      ? `Connected to hya ${bootstrap.location?.version ?? ""} · /help for commands`
      : `Connected to hya ${bootstrap.location?.version ?? ""} · key listing needs backend 0.41.0+`)
  } catch (error) {
    showStatus(`Connection failed: ${String(error)} · start hya serve`)
    view = "help"
    repaint()
  }
}

void main().catch((error: unknown) => {
  process.stderr.write(`hya-tui: ${String(error)}\n`)
  process.exitCode = 1
})
