import {
  batch,
  createContext,
  createEffect,
  createMemo,
  createSignal,
  For,
  Index,
  Match,
  on,
  onCleanup,
  onMount,
  Show,
  Switch,
  untrack,
  useContext,
} from "solid-js"
import { Dynamic } from "solid-js/web"
import path from "node:path"
import { mkdir, writeFile } from "node:fs/promises"
import { useRoute, useRouteData } from "../../context/route"
import { useProject } from "../../context/project"
import { useSync } from "../../context/sync"
import { useEvent } from "../../context/event"
import { SplitBorder } from "../../ui/border"
import { useTuiPaths, useTuiTerminalEnvironment } from "../../context/runtime"
import { Spinner } from "../../component/spinner"
import { createSyntaxStyleMemo, generateSubtleSyntax, selectedForeground, useTheme } from "../../context/theme"
import { BoxRenderable, ScrollBoxRenderable, addDefaultParsers, TextAttributes, RGBA } from "@opentui/core"
import { Prompt, type PromptRef } from "../../component/prompt"
import type {
  AssistantMessage,
  Part,
  Provider,
  ToolPart,
  UserMessage,
  TextPart,
  ReasoningPart,
} from "@opencode-ai/sdk/v2"
import { useLocal } from "../../context/local"
import { Locale } from "../../util/locale"
import { webSearchProviderLabel } from "../../util/tool-display"
import { useKeyboard, useRenderer, useTerminalDimensions, type JSX } from "@opentui/solid"
import { useSDK } from "../../context/sdk"
import { useEditorContext } from "../../context/editor"
import { openEditor } from "../../editor"
import { useDialog } from "../../ui/dialog"
import { DialogAlert } from "../../ui/dialog-alert"
import { TodoItem } from "../../component/todo-item"
import { DialogMessage } from "./dialog-message"
import type { PromptInfo } from "../../component/prompt/history"
import { DialogConfirm } from "../../ui/dialog-confirm"
import { DialogTimeline } from "./dialog-timeline"
import { DialogForkFromTimeline } from "./dialog-fork-from-timeline"
import { DialogSessionRename } from "../../component/dialog-session-rename"
import { Sidebar } from "./sidebar"
import { filetype } from "../../util/filetype"
import parsers from "../../parsers-config"
import { errorMessage } from "../../util/error"
import { Toast, useToast } from "../../ui/toast"
import { useKV } from "../../context/kv.tsx"
import stripAnsi from "strip-ansi"
import { usePromptRef } from "../../context/prompt"
import { useEpilogue } from "../../context/epilogue"
import { normalizePath } from "../../util/path"
import { PermissionPrompt } from "./permission"
import { QuestionPrompt } from "./question"
import { DialogExportOptions } from "../../ui/dialog-export-options"
import * as Model from "../../util/model"
import { formatTranscript } from "../../util/transcript"
import { sessionEpilogue } from "../../util/presentation"
import { setPreLayoutSiblingMargin } from "../../util/layout"
import { useTuiConfig } from "../../config"
import { useClipboard } from "../../context/clipboard"
import { nextThinkingMode, reasoningSummary, useThinkingMode, type ThinkingMode } from "../../context/thinking"
import { getScrollAcceleration } from "../../util/scroll"
import { collapseToolOutput } from "../../util/collapse-tool-output"
import { usePluginRuntime } from "../../plugin/runtime"
import { getRevertDiffFiles } from "../../util/revert-diff"
import {
  OPENCODE_BASE_MODE,
  useBindings,
  useCommandShortcut,
  useLeaderActive,
  useOpencodeKeymap,
} from "../../keymap"
import { PathFormatterProvider, usePathFormatter } from "../../context/path-format"
import { DialogSubagent, type SubagentPlacement } from "./dialog-subagent"
import {
  createRunTreeLoader,
  createWorkspaceState,
  flattenRunTree,
  reduceWorkspace,
  resolveLifecyclePresentation,
  runTreeEventEffect,
  treeSessionIDs,
  workspaceLeaves,
  workspacePaneStrip,
  type RunTreeNode,
  type RunTreeResource,
  type WorkspaceAction,
  type WorkspacePane,
} from "./subagent-workspace"
import {
  launchedMembersFromTree,
  resolveTaskMembers,
  resolveTaskSessionId,
  type TaskMemberView,
} from "./task-presentation"

addDefaultParsers(parsers.parsers)

export const alwaysSeparate = new WeakSet<BoxRenderable>()

const sessionBindingCommands = [
  "session.rename",
  "session.timeline",
  "session.fork",
  "session.compact",
  "session.undo",
  "session.redo",
  "session.sidebar.toggle",
  "session.toggle.conceal",
  "session.toggle.timestamps",
  "session.toggle.thinking",
  "session.toggle.actions",
  "session.toggle.scrollbar",
  "session.toggle.generic_tool_output",
  "session.first",
  "session.last",
  "session.messages_last_user",
  "session.message.next",
  "session.message.previous",
  "messages.copy",
  "session.copy",
  "session.export",
  "pane.roster",
  "pane.open.tab",
  "pane.open.vertical",
  "pane.open.horizontal",
  "pane.close",
  "pane.cycle",
  "pane.focus.main",
] as const

const sessionGlobalBindingCommands = [
  "session.page.up",
  "session.page.down",
  "session.line.up",
  "session.line.down",
  "session.half.page.up",
  "session.half.page.down",
] as const

const sessionGlobalUnfocusedBindingCommands = ["session.first", "session.last"] as const

const childRouteBlockedCommands: ReadonlySet<string> = new Set([
  "session.rename",
  "session.fork",
  "session.compact",
  "session.undo",
  "session.redo",
  "session.background",
])

const context = createContext<{
  width: number
  sessionID: string
  conceal: () => boolean
  thinkingMode: () => ThinkingMode
  showThinking: () => boolean
  showTimestamps: () => boolean
  showDetails: () => boolean
  showGenericToolOutput: () => boolean
  diffWrapMode: () => "word" | "none"
  providers: () => ReadonlyMap<string, Provider>
  openSubagent: (sessionID: string) => void
  /** Live subagent run tree for the focused main session (OpenCode-style status). */
  runTree: () => RunTreeNode | undefined
  sync: ReturnType<typeof useSync>
  tui: ReturnType<typeof useTuiConfig>
}>()

function use() {
  const ctx = useContext(context)
  if (!ctx) throw new Error("useContext must be used within a Session component")
  return ctx
}

export function Session() {
  const setEpilogue = useEpilogue()
  const clipboard = useClipboard()
  const writeExport = async (file: string, content: string) => {
    await mkdir(path.dirname(file), { recursive: true })
    await writeFile(file, content)
  }
  const pluginRuntime = usePluginRuntime()
  const route = useRouteData("session")
  const { navigate } = useRoute()
  const sync = useSync()
  const event = useEvent()
  const [treeResource, setTreeResource] = createSignal<RunTreeResource>({ status: "loading" })
  const [workspace, setWorkspace] = createSignal(createWorkspaceState(route.sessionID))
  const project = useProject()
  const paths = useTuiPaths()
  const tuiConfig = useTuiConfig()
  const kv = useKV()
  const { theme } = useTheme()
  const promptRef = usePromptRef()
  const session = createMemo(() => sync.session.get(route.sessionID))
  const mainSessionInteractive = createMemo(() => !!session() && !session()?.parentID)

  createEffect(() => {
    const title = Locale.truncate(session()?.title ?? "", 50)
    setEpilogue(sessionEpilogue({ title, sessionID: session()?.id }))
  })
  onCleanup(() => setEpilogue())
  const children = createMemo(() => {
    const parentID = session()?.parentID ?? session()?.id
    return sync.data.session
      .filter((x) => x.parentID === parentID || x.id === parentID)
      .toSorted((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
  })
  const messages = createMemo(() => sync.data.message[route.sessionID] ?? [])
  const foregroundTasks = createMemo(() =>
    sync.data.capabilities.experimentalBackgroundSubagents
      ? messages().flatMap((message) =>
          (sync.data.part[message.id] ?? []).filter(
            (part): part is ToolPart =>
              part.type === "tool" &&
              part.tool === "task" &&
              part.state.status === "running" &&
              part.state.metadata?.background !== true,
          ),
        )
      : [],
  )
  const permissions = createMemo(() => {
    if (session()?.parentID) return []
    const tree = treeResource().tree
    const sessionIDs = tree ? treeSessionIDs(tree) : new Set(children().map((child) => child.id))
    return [...sessionIDs].flatMap((sessionID) => sync.data.permission[sessionID] ?? [])
  })
  const questions = createMemo(() => {
    if (session()?.parentID) return []
    const tree = treeResource().tree
    const sessionIDs = tree ? treeSessionIDs(tree) : new Set(children().map((child) => child.id))
    return [...sessionIDs].flatMap((sessionID) => sync.data.question[sessionID] ?? [])
  })
  const visible = createMemo(() => !session()?.parentID && permissions().length === 0 && questions().length === 0)
  const disabled = createMemo(() => permissions().length > 0 || questions().length > 0)

  const pending = createMemo(() => {
    const completed = messages().findLast((x) => x.role === "assistant" && x.time.completed)?.id
    return messages().findLast((x) => x.role === "assistant" && !x.time.completed && (!completed || x.id > completed))
      ?.id
  })

  const lastAssistant = createMemo(() => {
    return messages().findLast((x) => x.role === "assistant")
  })

  const dimensions = useTerminalDimensions()
  const [sidebar, setSidebar] = kv.signal<"auto" | "hide">("sidebar", "auto")
  const [sidebarOpen, setSidebarOpen] = createSignal(false)
  const [conceal, setConceal] = createSignal(true)
  const thinking = useThinkingMode()
  const thinkingMode = thinking.mode
  const showThinking = createMemo(() => true)
  const [timestamps, setTimestamps] = kv.signal<"hide" | "show">("timestamps", "hide")
  const [showDetails, setShowDetails] = kv.signal("tool_details_visibility", true)
  const [showAssistantMetadata, _setShowAssistantMetadata] = kv.signal("assistant_metadata_visibility", true)
  const [showScrollbar, setShowScrollbar] = kv.signal("scrollbar_visible", false)
  const [diffWrapMode] = kv.signal<"word" | "none">("diff_wrap_mode", "word")
  const [_animationsEnabled, _setAnimationsEnabled] = kv.signal("animations_enabled", true)
  const [showGenericToolOutput, setShowGenericToolOutput] = kv.signal("generic_tool_output_visibility", false)

  const wide = createMemo(() => dimensions().width > 120)
  const sidebarVisible = createMemo(() => {
    if (session()?.parentID) return false
    if (sidebarOpen()) return true
    if (sidebar() === "auto" && wide()) return true
    return false
  })
  const showTimestamps = createMemo(() => timestamps() === "show")
  const contentWidth = createMemo(() => dimensions().width - (sidebarVisible() ? 42 : 0) - 4)
  const providers = createMemo(() => Model.index(sync.data.provider))

  const scrollAcceleration = createMemo(() => getScrollAcceleration(tuiConfig))
  const toast = useToast()
  const sdk = useSDK()
  const editor = useEditorContext()
  const dispatchWorkspace = (action: WorkspaceAction) => setWorkspace((state) => reduceWorkspace(state, action))
  const treeLoader = createRunTreeLoader({
    fetchTree: async (sessionID) => {
      const response = await sdk.fetch(`${sdk.url}/session/${encodeURIComponent(sessionID)}/tree`)
      if (!response.ok) throw new Error(`Failed to load subagent tree: ${response.status}`)
      return response.json()
    },
    onState: setTreeResource,
    onTree: (tree) => {
      dispatchWorkspace({ type: "reconcileSessions", sessionIDs: [...treeSessionIDs(tree)] })
      if (tree.session && tree.session !== route.sessionID) {
        navigate({ type: "session", sessionID: tree.session })
      }
    },
  })
  treeLoader.setSession(route.sessionID)
  onMount(() => void treeLoader.refresh())
  onCleanup(
    event.subscribe((raw) => {
      const effect = runTreeEventEffect(raw)
      if (effect.refresh) void treeLoader.refresh()
    }),
  )
  const openSubagent = (sessionID: string, placement: SubagentPlacement) => {
    const tree = treeResource().tree
    if (!tree || !treeSessionIDs(tree).has(sessionID)) return
    // Selecting Main (tree root) always returns focus to the Main pane.
    if (sessionID === tree.session) {
      dispatchWorkspace({ type: "focusMain" })
      return
    }
    dispatchWorkspace(
      placement === "tab"
        ? { type: "openTab", sessionID }
        : { type: "openSplit", axis: placement, sessionID },
    )
  }
  const openSubagentDialog = (placement: SubagentPlacement) => {
    void treeLoader.refresh()
    dialog.replace(() => (
      <DialogSubagent
        resource={treeResource}
        placement={placement}
        retry={() => void treeLoader.refresh()}
        open={(sessionID, nextPlacement) => {
          openSubagent(sessionID, nextPlacement)
          dialog.clear()
          setTimeout(() => {
            if (!mainFocused()) prompt?.blur()
          }, 5)
        }}
        isOpen={(sessionID) => workspace().tabs.some((tab) => JSON.stringify(tab.root).includes(sessionID))}
        isFocused={(sessionID) => workspace().focusedPaneID === `observation:${sessionID}`}
      />
    ))
  }
  createEffect(() => {
    for (const pane of workspaceLeaves(workspace())) {
      if (pane.type === "observation") void sync.session.sync(pane.sessionID)
    }
  })

  createEffect(() => {
    const sessionID = route.sessionID
    void (async () => {
      const result = await sdk.client.session.get({ sessionID }, { throwOnError: true })
      if (!result.data) {
        toast.show({
          message: `Session not found: ${sessionID}`,
          variant: "error",
          duration: 5000,
        })
        navigate({ type: "home" })
        return
      }

      editor.reconnect(result.data.directory)
      await sync.session.sync(sessionID)
      if (route.sessionID === sessionID && scroll) scroll.scrollBy(100_000)
    })().catch((error) => {
      if (route.sessionID !== sessionID) return
      toast.show({
        message: errorMessage(error),
        variant: "error",
        duration: 5000,
      })
      navigate({ type: "home" })
    })
  })

  let lastSwitch: string | undefined = undefined
  event.on("message.part.updated", (evt) => {
    const part = evt.properties.part
    if (part.type !== "tool") return
    if (part.sessionID !== route.sessionID) return
    if (part.state.status !== "completed") return
    if (part.id === lastSwitch) return

    if (part.tool === "plan_exit") {
      local.agent.set("build")
      lastSwitch = part.id
    } else if (part.tool === "plan_enter") {
      local.agent.set("plan")
      lastSwitch = part.id
    }
  })

  let seeded = false
  let scroll: ScrollBoxRenderable
  const observationScrolls = new Map<string, ScrollBoxRenderable>()
  let prompt: PromptRef | undefined
  const mainFocused = createMemo(() => workspace().focusedPaneID === "main")
  const multiPane = createMemo(() => workspaceLeaves(workspace()).length > 1)
  const focusedScroll = () =>
    workspace().focusedPaneID === "main" ? scroll : observationScrolls.get(workspace().focusedPaneID)
  const focusedMessages = () => {
    const pane = workspaceLeaves(workspace()).find((candidate) => candidate.id === workspace().focusedPaneID)
    const sessionID = pane?.type === "observation" ? pane.sessionID : route.sessionID
    return sync.data.message[sessionID] ?? []
  }
  const bind = (r: PromptRef | undefined) => {
    prompt = r
    promptRef.set(r)
    if (r && !mainFocused()) r.blur()
    if (seeded || !route.prompt || !r) return
    seeded = true
    r.set(route.prompt)
  }
  const keymap = useOpencodeKeymap()
  const dialog = useDialog()
  const renderer = useRenderer()
  // Child session route (legacy / deep-link): Escape walks back to the parent.
  useBindings(() => ({
    enabled: () => !!session()?.parentID && mainFocused() && dialog.stack.length === 0,
    priority: 10,
    bindings: [
      {
        key: "escape",
        desc: "Return to parent session",
        group: "Session",
        cmd: () => {
          const parentID = session()?.parentID
          if (!parentID) return
          navigate({ type: "session", sessionID: parentID })
        },
      },
    ],
  }))
  const leaderActive = useLeaderActive()
  // Pane navigation is handled here (not only via keymap) so focus switching still
  // works when leader chords / command resolution miss — a real regression when
  // users open multiple subagent views. useKeyboard runs after the keymap.
  useKeyboard((key) => {
    if (dialog.stack.length > 0) return
    if (leaderActive()) return

    // Esc from any observation returns to Main (ADR-0003).
    if (!mainFocused() && key.name === "escape" && !key.ctrl && !key.meta && !key.option) {
      dispatchWorkspace({ type: "focusMain" })
      key.preventDefault()
      key.stopPropagation()
      return
    }

    // Left/Right cycle every open leaf (Main + all retained subagents). While Main
    // owns the prompt, require Ctrl so bare arrows still move the caret.
    if (multiPane() && (key.name === "left" || key.name === "right")) {
      if (!mainFocused() || key.ctrl) {
        dispatchWorkspace({ type: "cycleFocus", direction: key.name === "left" ? -1 : 1 })
        key.preventDefault()
        key.stopPropagation()
        return
      }
    }

    // Digit jump to pane strip: 1=Main, 2=first open subagent, …
    if (!mainFocused() && multiPane() && key.name.length === 1 && key.name >= "1" && key.name <= "9" && !key.ctrl && !key.meta && !key.option) {
      const entry = workspacePaneStrip(workspace())[Number(key.name) - 1]
      if (entry) {
        dispatchWorkspace({ type: "focus", paneID: entry.paneID })
        key.preventDefault()
        key.stopPropagation()
        return
      }
    }

    if (mainFocused()) return
    // Ignore bare typing while an observation pane is focused (read-only).
    if (key.name === "return" || (key.name.length === 1 && !key.ctrl && !key.meta && !key.option)) {
      key.preventDefault()
      key.stopPropagation()
    }
  })
  createEffect(() => {
    if (!prompt) return
    if (mainFocused()) prompt.focus()
    else prompt.blur()
  })

  // Helper: Find next visible message boundary in direction
  const findNextVisibleMessage = (direction: "next" | "prev"): string | null => {
    const target = focusedScroll()
    if (!target) return null
    const children = target.getChildren()
    const messagesList = focusedMessages()
    const scrollTop = target.y

    // Get visible messages sorted by position, filtering for valid non-synthetic, non-ignored content
    const visibleMessages = children
      .filter((c) => {
        if (!c.id) return false
        const message = messagesList.find((m) => m.id === c.id)
        if (!message) return false

        // Check if message has valid non-synthetic, non-ignored text parts
        const parts = sync.data.part[message.id]
        if (!parts || !Array.isArray(parts)) return false

        return parts.some((part) => part && part.type === "text" && !part.synthetic && !part.ignored)
      })
      .sort((a, b) => a.y - b.y)

    if (visibleMessages.length === 0) return null

    if (direction === "next") {
      // Find first message below current position
      return visibleMessages.find((c) => c.y > scrollTop + 10)?.id ?? null
    }
    // Find last message above current position
    return [...visibleMessages].reverse().find((c) => c.y < scrollTop - 10)?.id ?? null
  }

  // Helper: Scroll to message in direction or fallback to page scroll
  const scrollToMessage = (direction: "next" | "prev", dialog: ReturnType<typeof useDialog>) => {
    const target = focusedScroll()
    if (!target) return
    const targetID = findNextVisibleMessage(direction)

    if (!targetID) {
      target.scrollBy(direction === "next" ? target.height : -target.height)
      dialog.clear()
      return
    }

    const child = target.getChildren().find((c) => c.id === targetID)
    if (child) target.scrollBy(child.y - target.y - 1)
    dialog.clear()
  }

  function toBottom() {
    setTimeout(() => {
      if (!scroll || scroll.isDestroyed) return
      scroll.scrollTo(scroll.scrollHeight)
    }, 50)
  }

  const local = useLocal()

  const sessionCommandList = createMemo(() => [
    {
      title: "Rename session",
      value: "session.rename",
      category: "Session",
      slash: {
        name: "rename",
      },
      run: () => {
        dialog.replace(() => <DialogSessionRename session={route.sessionID} />)
      },
    },
    {
      title: "Jump to message",
      value: "session.timeline",
      category: "Session",
      slash: {
        name: "timeline",
      },
      run: () => {
        dialog.replace(() => (
          <DialogTimeline
            onMove={(messageID) => {
              const child = scroll.getChildren().find((child) => {
                return child.id === messageID
              })
              if (child) scroll.scrollBy(child.y - scroll.y - 1)
            }}
            sessionID={route.sessionID}
            setPrompt={(promptInfo) => prompt?.set(promptInfo)}
          />
        ))
      },
    },
    {
      title: "Fork session",
      value: "session.fork",
      category: "Session",
      slash: {
        name: "fork",
      },
      run: () => {
        dialog.replace(() => (
          <DialogForkFromTimeline
            onMove={(messageID) => {
              if (!messageID) return
              const child = scroll.getChildren().find((child) => {
                return child.id === messageID
              })
              if (child) scroll.scrollBy(child.y - scroll.y - 1)
            }}
            sessionID={route.sessionID}
          />
        ))
      },
    },
    {
      title: "Compact session",
      value: "session.compact",
      category: "Session",
      slash: {
        name: "compact",
        aliases: ["summarize"],
      },
      run: () => {
        const selectedModel = local.model.current()
        if (!selectedModel) {
          toast.show({
            variant: "warning",
            message: "Connect a provider to summarize this session",
            duration: 3000,
          })
          return
        }
        void sdk.client.session.summarize({
          sessionID: route.sessionID,
          modelID: selectedModel.modelID,
          providerID: selectedModel.providerID,
        })
        dialog.clear()
      },
    },
    {
      title: "Undo previous message",
      value: "session.undo",
      category: "Session",
      slash: {
        name: "undo",
      },
      run: async () => {
        const status = sync.data.session_status?.[route.sessionID]
        if (status?.type !== "idle") await sdk.client.session.abort({ sessionID: route.sessionID }).catch(() => {})
        const revert = session()?.revert?.messageID
        const message = messages().findLast((x) => (!revert || x.id < revert) && x.role === "user")
        if (!message) return
        void sdk.client.session
          .revert({
            sessionID: route.sessionID,
            messageID: message.id,
          })
          .then(() => {
            toBottom()
          })
        const parts = sync.data.part[message.id]
        prompt?.set(
          parts.reduce(
            (agg, part) => {
              if (part.type === "text") {
                if (!part.synthetic) agg.input += part.text
              }
              if (part.type === "file") agg.parts.push(part)
              return agg
            },
            { input: "", parts: [] as PromptInfo["parts"] },
          ),
        )
        dialog.clear()
      },
    },
    {
      title: "Redo",
      value: "session.redo",
      category: "Session",
      enabled: !!session()?.revert?.messageID,
      slash: {
        name: "redo",
      },
      run: () => {
        dialog.clear()
        const messageID = session()?.revert?.messageID
        if (!messageID) return
        const message = messages().find((x) => x.role === "user" && x.id > messageID)
        if (!message) {
          void sdk.client.session.unrevert({
            sessionID: route.sessionID,
          })
          prompt?.set({ input: "", parts: [] })
          return
        }
        void sdk.client.session.revert({
          sessionID: route.sessionID,
          messageID: message.id,
        })
      },
    },
    {
      title: sidebarVisible() ? "Hide sidebar" : "Show sidebar",
      value: "session.sidebar.toggle",
      category: "Session",
      run: () => {
        batch(() => {
          const isVisible = sidebarVisible()
          setSidebar(() => (isVisible ? "hide" : "auto"))
          setSidebarOpen(!isVisible)
        })
        dialog.clear()
      },
    },
    {
      title: conceal() ? "Disable code concealment" : "Enable code concealment",
      value: "session.toggle.conceal",
      category: "Session",
      run: () => {
        setConceal((prev) => !prev)
        dialog.clear()
      },
    },
    {
      title: showTimestamps() ? "Hide timestamps" : "Show timestamps",
      value: "session.toggle.timestamps",
      category: "Session",
      slash: {
        name: "timestamps",
        aliases: ["toggle-timestamps"],
      },
      run: () => {
        setTimestamps((prev) => (prev === "show" ? "hide" : "show"))
        dialog.clear()
      },
    },
    {
      title: (() => {
        const next = nextThinkingMode(thinkingMode())
        if (next === "hide") return "Collapse thinking"
        return "Expand thinking"
      })(),
      value: "session.toggle.thinking",
      category: "Session",
      slash: {
        name: "thinking",
        aliases: ["toggle-thinking"],
      },
      run: () => {
        thinking.set(nextThinkingMode(thinkingMode()))
        dialog.clear()
      },
    },
    {
      title: showDetails() ? "Hide tool details" : "Show tool details",
      value: "session.toggle.actions",
      category: "Session",
      run: () => {
        setShowDetails((prev) => !prev)
        dialog.clear()
      },
    },
    {
      title: "Toggle session scrollbar",
      value: "session.toggle.scrollbar",
      category: "Session",
      run: () => {
        setShowScrollbar((prev) => !prev)
        dialog.clear()
      },
    },
    {
      title: showGenericToolOutput() ? "Hide generic tool output" : "Show generic tool output",
      value: "session.toggle.generic_tool_output",
      category: "Session",
      run: () => {
        setShowGenericToolOutput((prev) => !prev)
        dialog.clear()
      },
    },
    {
      title: "Page up",
      value: "session.page.up",
      category: "Session",
      hidden: true,
      run: () => {
        const target = focusedScroll()
        if (target) target.scrollBy(-target.height / 2)
        dialog.clear()
      },
    },
    {
      title: "Page down",
      value: "session.page.down",
      category: "Session",
      hidden: true,
      run: () => {
        const target = focusedScroll()
        if (target) target.scrollBy(target.height / 2)
        dialog.clear()
      },
    },
    {
      title: "Line up",
      value: "session.line.up",
      category: "Session",
      hidden: true,
      run: () => {
        focusedScroll()?.scrollBy(-1)
        dialog.clear()
      },
    },
    {
      title: "Line down",
      value: "session.line.down",
      category: "Session",
      hidden: true,
      run: () => {
        focusedScroll()?.scrollBy(1)
        dialog.clear()
      },
    },
    {
      title: "Half page up",
      value: "session.half.page.up",
      category: "Session",
      hidden: true,
      run: () => {
        const target = focusedScroll()
        if (target) target.scrollBy(-target.height / 4)
        dialog.clear()
      },
    },
    {
      title: "Half page down",
      value: "session.half.page.down",
      category: "Session",
      hidden: true,
      run: () => {
        const target = focusedScroll()
        if (target) target.scrollBy(target.height / 4)
        dialog.clear()
      },
    },
    {
      title: "First message",
      value: "session.first",
      category: "Session",
      hidden: true,
      run: () => {
        focusedScroll()?.scrollTo(0)
        dialog.clear()
      },
    },
    {
      title: "Last message",
      value: "session.last",
      category: "Session",
      hidden: true,
      run: () => {
        const target = focusedScroll()
        if (target) target.scrollTo(target.scrollHeight)
        dialog.clear()
      },
    },
    {
      title: "Jump to last user message",
      value: "session.messages_last_user",
      category: "Session",
      hidden: true,
      run: () => {
        const paneMessages = focusedMessages()
        if (!paneMessages.length) return
        const target = focusedScroll()
        if (!target) return

        // Find the most recent user message with non-ignored, non-synthetic text parts
        for (let i = paneMessages.length - 1; i >= 0; i--) {
          const message = paneMessages[i]
          if (!message || message.role !== "user") continue

          const parts = sync.data.part[message.id]
          if (!parts || !Array.isArray(parts)) continue

          const hasValidTextPart = parts.some(
            (part) => part && part.type === "text" && !part.synthetic && !part.ignored,
          )

          if (hasValidTextPart) {
            const child = target.getChildren().find((child) => {
              return child.id === message.id
            })
            if (child) target.scrollBy(child.y - target.y - 1)
            break
          }
        }
      },
    },
    {
      title: "Next message",
      value: "session.message.next",
      category: "Session",
      hidden: true,
      run: () => scrollToMessage("next", dialog),
    },
    {
      title: "Previous message",
      value: "session.message.previous",
      category: "Session",
      hidden: true,
      run: () => scrollToMessage("prev", dialog),
    },
    {
      title: "Copy last assistant message",
      value: "messages.copy",
      category: "Session",
      run: () => {
        const revertID = session()?.revert?.messageID
        const lastAssistantMessage = messages().findLast(
          (msg) => msg.role === "assistant" && (!revertID || msg.id < revertID),
        )
        if (!lastAssistantMessage) {
          toast.show({ message: "No assistant messages found", variant: "error" })
          dialog.clear()
          return
        }

        const parts = sync.data.part[lastAssistantMessage.id] ?? []
        const textParts = parts.filter((part) => part.type === "text")
        if (textParts.length === 0) {
          toast.show({ message: "No text parts found in last assistant message", variant: "error" })
          dialog.clear()
          return
        }

        const text = textParts
          .map((part) => part.text)
          .join("\n")
          .trim()
        if (!text) {
          toast.show({
            message: "No text content found in last assistant message",
            variant: "error",
          })
          dialog.clear()
          return
        }

        clipboard
          .write?.(text)
          .then(() => toast.show({ message: "Message copied to clipboard!", variant: "success" }))
          .catch(() => toast.show({ message: "Failed to copy to clipboard", variant: "error" }))
        dialog.clear()
      },
    },
    {
      title: "Copy session transcript",
      value: "session.copy",
      category: "Session",
      slash: {
        name: "copy",
      },
      run: async () => {
        try {
          const sessionData = session()
          if (!sessionData) return
          const sessionMessages = messages()
          const transcript = formatTranscript(
            sessionData,
            sessionMessages.map((msg) => ({ info: msg, parts: sync.data.part[msg.id] ?? [] })),
            {
              thinking: showThinking(),
              toolDetails: showDetails(),
              assistantMetadata: showAssistantMetadata(),
              providers: sync.data.provider,
            },
          )
          await clipboard.write?.(transcript)
          toast.show({ message: "Session transcript copied to clipboard!", variant: "success" })
        } catch {
          toast.show({ message: "Failed to copy session transcript", variant: "error" })
        }
        dialog.clear()
      },
    },
    {
      title: "Export session transcript",
      value: "session.export",
      category: "Session",
      slash: {
        name: "export",
      },
      run: async () => {
        try {
          const sessionData = session()
          if (!sessionData) return
          const sessionMessages = messages()

          const defaultFilename = `session-${sessionData.id.slice(0, 8)}.md`

          const options = await DialogExportOptions.show(
            dialog,
            defaultFilename,
            showThinking(),
            showDetails(),
            showAssistantMetadata(),
            false,
          )

          if (options === null) return

          const transcript = formatTranscript(
            sessionData,
            sessionMessages.map((msg) => ({ info: msg, parts: sync.data.part[msg.id] ?? [] })),
            {
              thinking: options.thinking,
              toolDetails: options.toolDetails,
              assistantMetadata: options.assistantMetadata,
              providers: sync.data.provider,
            },
          )

          if (options.openWithoutSaving) {
            // Just open in editor without saving
            await openEditor({
              renderer,
              value: transcript,
              cwd:
                (project.instance.path().worktree === "/" ? undefined : project.instance.path().worktree) ||
                project.instance.directory() ||
                paths.cwd,
            })
          } else {
            const exportDir = paths.cwd
            const filename = options.filename.trim()
            const filepath = path.join(exportDir, filename)

            await writeExport(filepath, transcript)

            // Open with EDITOR if available
            const result = await openEditor({
              renderer,
              value: transcript,
              cwd:
                (project.instance.path().worktree === "/" ? undefined : project.instance.path().worktree) ||
                project.instance.directory() ||
                paths.cwd,
            })
            if (result !== undefined) {
              await writeExport(filepath, result)
            }

            toast.show({ message: `Session exported to ${filename}`, variant: "success" })
          }
        } catch {
          toast.show({ message: "Failed to export session", variant: "error" })
        }
        dialog.clear()
      },
    },
    {
      title: "Background subagents",
      value: "session.background",
      category: "Session",
      hidden: true,
      enabled: foregroundTasks().length > 0,
      run: () => {
        void sdk.client.experimental.session.background({
          sessionID: route.sessionID,
          workspace: project.workspace.current(),
        })
        dialog.clear()
      },
    },
    {
      title: "Open subagent roster",
      value: "pane.roster",
      category: "Pane",
      run: () => openSubagentDialog("tab"),
    },
    {
      title: "Open subagent in tab",
      value: "pane.open.tab",
      category: "Pane",
      run: () => openSubagentDialog("tab"),
    },
    {
      title: "Open subagent in vertical split",
      value: "pane.open.vertical",
      category: "Pane",
      run: () => openSubagentDialog("vertical"),
    },
    {
      title: "Open subagent in horizontal split",
      value: "pane.open.horizontal",
      category: "Pane",
      run: () => openSubagentDialog("horizontal"),
    },
    {
      title: "Close focused pane",
      value: "pane.close",
      category: "Pane",
      run: () => dispatchWorkspace({ type: "close", paneID: workspace().focusedPaneID }),
    },
    {
      title: "Cycle pane focus",
      value: "pane.cycle",
      category: "Pane",
      run: () => dispatchWorkspace({ type: "cycleFocus", direction: 1 }),
    },
    {
      title: "Focus Main pane",
      value: "pane.focus.main",
      category: "Pane",
      run: () => dispatchWorkspace({ type: "focusMain" }),
    },
  ])

  const sessionCommands = createMemo(() =>
    sessionCommandList()
      .filter((command) => mainSessionInteractive() || !childRouteBlockedCommands.has(command.value))
      .map((command) => ({
        namespace: "palette",
        name: command.value,
        desc: "description" in command ? command.description : undefined,
        slashName: "slash" in command ? command.slash?.name : undefined,
        slashAliases: "slash" in command ? command.slash?.aliases : undefined,
        ...command,
      })),
  )

  useBindings(() => ({
    commands: sessionCommands(),
  }))

  useBindings(() => ({
    bindings: tuiConfig.keybinds.gather("session.global", sessionGlobalBindingCommands),
  }))

  useBindings(() => ({
    enabled: () => renderer.currentFocusedEditor === null,
    bindings: tuiConfig.keybinds.gather("session.global.unfocused", sessionGlobalUnfocusedBindingCommands),
  }))

  useBindings(() => ({
    mode: OPENCODE_BASE_MODE,
    bindings: tuiConfig.keybinds.gather(
      "session",
      sessionBindingCommands.filter(
        (command) => mainSessionInteractive() || !childRouteBlockedCommands.has(command),
      ),
    ),
  }))

  useBindings(() => ({
    mode: OPENCODE_BASE_MODE,
    enabled: mainSessionInteractive() && foregroundTasks().length > 0,
    priority: 1,
    bindings: tuiConfig.keybinds.get("session.background"),
  }))

  useBindings(() => ({
    enabled: !!session()?.parentID && treeResource().status === "error" && dialog.stack.length === 0,
    bindings: [
      {
        key: "r",
        desc: "Retry subagent tree",
        group: "Pane",
        cmd: () => void treeLoader.refresh(),
      },
    ],
  }))

  const revertInfo = createMemo(() => session()?.revert)
  const revertMessageID = createMemo(() => revertInfo()?.messageID)

  const revertDiffFiles = createMemo(() => getRevertDiffFiles(revertInfo()?.diff ?? ""))

  const revertRevertedMessages = createMemo(() => {
    const messageID = revertMessageID()
    if (!messageID) return []
    return messages().filter((x) => x.id >= messageID && x.role === "user")
  })

  const revert = createMemo(() => {
    const info = revertInfo()
    if (!info) return
    if (!info.messageID) return
    return {
      messageID: info.messageID,
      reverted: revertRevertedMessages(),
      diff: info.diff,
      diffFiles: revertDiffFiles(),
    }
  })

  // snap to bottom when session changes
  createEffect(on(() => route.sessionID, toBottom))

  const paneStrip = createMemo(() => {
    const tree = treeResource().tree
    const rows = tree ? flattenRunTree(tree) : []
    return workspacePaneStrip(workspace()).map((entry) => {
      if (entry.paneID === "main") return { ...entry, label: "main" }
      const sessionID = entry.paneID.startsWith("observation:")
        ? entry.paneID.slice("observation:".length)
        : entry.paneID
      const node = rows.find((row) => row.node.session === sessionID)?.node
      return {
        ...entry,
        label: node?.roster?.handle ?? node?.member?.subagent_type ?? sessionID.slice(0, 12),
      }
    })
  })
  const focusPaneByID = (paneID: string) => dispatchWorkspace({ type: "focus", paneID })
  const observationPresentation = (sessionID: string, placement: SubagentPlacement, focused: boolean) => {
    const tree = treeResource().tree
    const node = tree && flattenRunTree(tree).find((row) => row.node.session === sessionID)?.node
    const lifecycle = resolveLifecyclePresentation(node ?? {})
    return {
      label: [
        node?.roster?.handle ?? node?.member?.subagent_type ?? "subagent",
        node?.roster?.agent_type ?? node?.member?.subagent_type ?? node?.agent,
        lifecycle.label,
        node?.roster?.current_task ?? node?.member?.description,
        placement,
        focused ? "focused" : "open",
        "read-only",
        focused ? "←/→ or 1-9 panes · esc main · ctrl+x w close" : undefined,
      ]
        .filter(Boolean)
        .join(" - "),
      working: lifecycle.working,
    }
  }
  function ObservationTranscript(props: { paneID: string; sessionID: string; placement: SubagentPlacement }) {
    const paneMessages = createMemo(() => sync.data.message[props.sessionID] ?? [])
    const panePending = createMemo(() => {
      const completed = paneMessages().findLast((message) => message.role === "assistant" && message.time.completed)?.id
      return paneMessages().findLast(
        (message) => message.role === "assistant" && !message.time.completed && (!completed || message.id > completed),
      )?.id
    })
    const paneLastAssistant = createMemo(() => paneMessages().findLast((message) => message.role === "assistant")?.id)
    const presentation = createMemo(() =>
      observationPresentation(props.sessionID, props.placement, props.paneID === workspace().focusedPaneID),
    )
    onCleanup(() => observationScrolls.delete(props.paneID))
    return (
      <box flexGrow={1} minWidth={0} minHeight={0} gap={1}>
        <box flexDirection="row" gap={1} minWidth={0}>
          <Show when={presentation().working}>
            <Spinner color={theme.textMuted} />
          </Show>
          <text fg={theme.textMuted} wrapMode="word" flexGrow={1} minWidth={0}>
            {presentation().label}
          </text>
        </box>
        <scrollbox
          ref={(value) => observationScrolls.set(props.paneID, value)}
          stickyScroll
          stickyStart="bottom"
          flexGrow={1}
          scrollAcceleration={scrollAcceleration()}
        >
          <box height={1} />
          <For each={paneMessages()}>
            {(message, index) => (
              <Switch>
                <Match when={message.role === "user"}>
                  <UserMessage
                    index={index()}
                    onMouseUp={() => {}}
                    message={message as UserMessage}
                    parts={sync.data.part[message.id] ?? []}
                    pending={panePending()}
                  />
                </Match>
                <Match when={message.role === "assistant"}>
                  <AssistantMessage
                    last={paneLastAssistant() === message.id}
                    message={message as AssistantMessage}
                    parts={sync.data.part[message.id] ?? []}
                  />
                </Match>
              </Switch>
            )}
          </For>
        </scrollbox>
      </box>
    )
  }
  function WorkspacePaneView(props: {
    pane: () => WorkspacePane
    placement: SubagentPlacement
    renderMain: () => JSX.Element
  }) {
    const split = createMemo(() => {
      const pane = props.pane()
      return pane.type === "split" ? pane : undefined
    })
    const observation = createMemo(() => {
      const pane = props.pane()
      return pane.type === "observation" ? pane : undefined
    })
    return (
      <Switch fallback={props.renderMain()}>
        <Match when={split()}>
          {(pane) => (
            <box
              flexDirection={pane().axis === "vertical" ? "row" : "column"}
              flexGrow={1}
              minWidth={0}
              minHeight={0}
            >
              <WorkspacePaneView pane={() => pane().first} placement={pane().axis} renderMain={props.renderMain} />
              <WorkspacePaneView pane={() => pane().second} placement={pane().axis} renderMain={props.renderMain} />
            </box>
          )}
        </Match>
        <Match when={observation()}>
          {(pane) => (
            <box
              flexGrow={1}
              minWidth={0}
              minHeight={0}
              paddingLeft={1}
              paddingRight={1}
              backgroundColor={pane().id === workspace().focusedPaneID ? theme.backgroundElement : theme.backgroundPanel}
              onMouseDown={() => focusPaneByID(pane().id)}
              onMouseUp={() => focusPaneByID(pane().id)}
            >
              <ObservationTranscript paneID={pane().id} sessionID={pane().sessionID} placement={props.placement} />
            </box>
          )}
        </Match>
      </Switch>
    )
  }

  return (
    <PathFormatterProvider path={session()?.directory}>
      <context.Provider
        value={{
          get width() {
            return contentWidth()
          },
          sessionID: route.sessionID,
          conceal,
          thinkingMode,
          showThinking,
          showTimestamps,
          showDetails,
          showGenericToolOutput,
          diffWrapMode,
          providers,
          openSubagent: (sessionID) => openSubagent(sessionID, "tab"),
          runTree: () => treeResource().tree,
          sync,
          tui: tuiConfig,
        }}
      >
        <box flexDirection="column" flexGrow={1} minHeight={0}>
          <Show when={multiPane()}>
            <box flexDirection="row" flexShrink={0} gap={1} paddingLeft={1} paddingRight={1} paddingBottom={1}>
              <For each={paneStrip()}>
                {(entry) => (
                  <box
                    paddingLeft={1}
                    paddingRight={1}
                    backgroundColor={entry.focused ? theme.accent : theme.backgroundPanel}
                    onMouseDown={() => focusPaneByID(entry.paneID)}
                    onMouseUp={() => focusPaneByID(entry.paneID)}
                  >
                    <text fg={entry.focused ? theme.background : theme.textMuted}>
                      {`${paneStrip().findIndex((item) => item.paneID === entry.paneID) + 1}:${entry.label}`}
                    </text>
                  </box>
                )}
              </For>
            </box>
          </Show>
          <box flexDirection="row" flexGrow={1} minHeight={0}>
          <Index each={workspace().tabs}>
            {(tab) => (
              <box
                visible={workspace().activeTabID === tab().id}
                flexGrow={1}
                minWidth={0}
                minHeight={0}
              >
                <WorkspacePaneView
                  pane={() => tab().root}
                  placement="tab"
                  renderMain={() => (
                    <box
                      flexGrow={1}
                      minWidth={0}
                      minHeight={0}
                      paddingBottom={1}
                      paddingLeft={2}
                      paddingRight={2}
                      gap={1}
                      backgroundColor={
                        multiPane()
                          ? mainFocused()
                            ? theme.backgroundElement
                            : theme.backgroundPanel
                          : undefined
                      }
                      onMouseDown={() => focusPaneByID("main")}
                      onMouseUp={() => focusPaneByID("main")}
                    >
            <Show when={session()}>
              <scrollbox
                ref={(r) => (scroll = r)}
                viewportOptions={{
                  paddingRight: showScrollbar() ? 1 : 0,
                }}
                verticalScrollbarOptions={{
                  paddingLeft: 1,
                  visible: showScrollbar(),
                  trackOptions: {
                    backgroundColor: theme.backgroundElement,
                    foregroundColor: theme.border,
                  },
                }}
                stickyScroll={true}
                stickyStart="bottom"
                flexGrow={1}
                scrollAcceleration={scrollAcceleration()}
              >
                <box height={1} />
                <For each={messages()}>
                  {(message, index) => (
                    <Switch>
                      <Match when={message.id === revert()?.messageID}>
                        {(function () {
                          const redoShortcut = useCommandShortcut("session.redo")
                          const [hover, setHover] = createSignal(false)
                          const dialog = useDialog()

                          const handleUnrevert = async () => {
                            const confirmed = await DialogConfirm.show(
                              dialog,
                              "Confirm Redo",
                              "Are you sure you want to restore the reverted messages?",
                            )
                            if (confirmed) {
                              keymap.dispatchCommand("session.redo")
                            }
                          }

                          return (
                            <box
                              onMouseOver={() => setHover(true)}
                              onMouseOut={() => setHover(false)}
                              onMouseUp={handleUnrevert}
                              marginTop={1}
                              flexShrink={0}
                              border={["left"]}
                              customBorderChars={SplitBorder.customBorderChars}
                              borderColor={theme.backgroundPanel}
                            >
                              <box
                                paddingTop={1}
                                paddingBottom={1}
                                paddingLeft={2}
                                backgroundColor={hover() ? theme.backgroundElement : theme.backgroundPanel}
                              >
                                <text fg={theme.textMuted}>{revert()!.reverted.length} message reverted</text>
                                <text fg={theme.textMuted}>
                                  <span style={{ fg: theme.text }}>{redoShortcut()}</span> or /redo to restore
                                </text>
                                <Show when={revert()!.diffFiles?.length}>
                                  <box marginTop={1}>
                                    <For each={revert()!.diffFiles}>
                                      {(file) => (
                                        <text fg={theme.text}>
                                          {file.filename}
                                          <Show when={file.additions > 0}>
                                            <span style={{ fg: theme.diffAdded }}> +{file.additions}</span>
                                          </Show>
                                          <Show when={file.deletions > 0}>
                                            <span style={{ fg: theme.diffRemoved }}> -{file.deletions}</span>
                                          </Show>
                                        </text>
                                      )}
                                    </For>
                                  </box>
                                </Show>
                              </box>
                            </box>
                          )
                        })()}
                      </Match>
                      <Match when={revert()?.messageID && message.id >= revert()!.messageID}>
                        <></>
                      </Match>
                      <Match when={message.role === "user"}>
                        <UserMessage
                          index={index()}
                          onMouseUp={() => {
                            if (renderer.getSelection()?.getSelectedText()) return
                            dialog.replace(() => (
                              <DialogMessage
                                messageID={message.id}
                                sessionID={route.sessionID}
                                setPrompt={(promptInfo) => prompt?.set(promptInfo)}
                              />
                            ))
                          }}
                          message={message as UserMessage}
                          parts={sync.data.part[message.id] ?? []}
                          pending={pending()}
                        />
                      </Match>
                      <Match when={message.role === "assistant"}>
                        <AssistantMessage
                          last={lastAssistant()?.id === message.id}
                          message={message as AssistantMessage}
                          parts={sync.data.part[message.id] ?? []}
                        />
                      </Match>
                    </Switch>
                  )}
                </For>
              </scrollbox>
              <box flexShrink={0}>
                <Show when={mainFocused() && permissions().length > 0}>
                  <PermissionPrompt
                    request={permissions()[0]}
                    directory={sync.session.get(permissions()[0].sessionID)?.directory}
                  />
                </Show>
                <Show when={mainFocused() && permissions().length === 0 && questions().length > 0}>
                  <QuestionPrompt
                    request={questions()[0]}
                    directory={sync.session.get(questions()[0].sessionID)?.directory}
                  />
                </Show>
                <Show when={session()?.parentID && treeResource().status === "error"}>
                  <text fg={theme.textMuted}>Subagent tree unavailable - press r to retry</text>
                </Show>
                <Show when={visible()}>
                  <pluginRuntime.Slot
                    name="session_prompt"
                    mode="replace"
                    session_id={route.sessionID}
                    visible={visible() && mainFocused()}
                    disabled={disabled()}
                    on_submit={toBottom}
                    ref={bind}
                  >
                    <Prompt
                      visible={visible() && mainFocused()}
                      ref={bind}
                      disabled={disabled()}
                      onSubmit={() => {
                        toBottom()
                      }}
                      sessionID={route.sessionID}
                      right={<pluginRuntime.Slot name="session_prompt_right" session_id={route.sessionID} />}
                    />
                  </pluginRuntime.Slot>
                </Show>
              </box>
            </Show>
                      <Toast />
                    </box>
                  )}
                />
              </box>
            )}
          </Index>
          <Show when={sidebarVisible()}>
            <Switch>
              <Match when={wide()}>
                <Sidebar sessionID={route.sessionID} />
              </Match>
              <Match when={!wide()}>
                <box
                  position="absolute"
                  top={0}
                  left={0}
                  right={0}
                  bottom={0}
                  alignItems="flex-end"
                  backgroundColor={RGBA.fromInts(0, 0, 0, 70)}
                >
                  <Sidebar sessionID={route.sessionID} />
                </box>
              </Match>
            </Switch>
          </Show>
          </box>
        </box>
      </context.Provider>
    </PathFormatterProvider>
  )
}

const MIME_BADGE: Record<string, string> = {
  "text/plain": "txt",
  "image/png": "img",
  "image/jpeg": "img",
  "image/gif": "img",
  "image/webp": "img",
  "application/pdf": "pdf",
  "application/x-directory": "dir",
}

function UserMessage(props: {
  message: UserMessage
  parts: Part[]
  onMouseUp: () => void
  index: number
  pending?: string
}) {
  const ctx = use()
  const local = useLocal()
  const text = createMemo(() => {
    const texts = props.parts
      .map((x) => {
        if (x.type === "text" && !x.synthetic) {
          return x.text
        }
        return null
      })
      .filter(Boolean)
    return texts.join("\n\n")
  })
  const files = createMemo(() => props.parts.flatMap((x) => (x.type === "file" ? [x] : [])))
  const { theme } = useTheme()
  const [hover, setHover] = createSignal(false)
  const queued = createMemo(() => props.pending && props.message.id > props.pending)
  const color = createMemo(() => local.agent.color(props.message.agent))
  const queuedFg = createMemo(() => selectedForeground(theme, color()))
  const metadataVisible = createMemo(() => queued() || ctx.showTimestamps())

  const compaction = createMemo(() => props.parts.find((x) => x.type === "compaction"))

  return (
    <>
      <Show when={text()}>
        <box
          id={props.message.id}
          ref={(el: BoxRenderable) => alwaysSeparate.add(el)}
          border={["left"]}
          borderColor={color()}
          customBorderChars={SplitBorder.customBorderChars}
          marginTop={props.index === 0 ? 0 : 1}
        >
          <box
            onMouseOver={() => {
              setHover(true)
            }}
            onMouseOut={() => {
              setHover(false)
            }}
            onMouseUp={props.onMouseUp}
            paddingTop={1}
            paddingBottom={1}
            paddingLeft={2}
            backgroundColor={hover() ? theme.backgroundElement : theme.backgroundPanel}
            flexShrink={0}
          >
            <text fg={theme.text}>{text()}</text>
            <Show when={files().length}>
              <box flexDirection="row" paddingBottom={metadataVisible() ? 1 : 0} paddingTop={1} gap={1} flexWrap="wrap">
                <For each={files()}>
                  {(file) => {
                    const bg = createMemo(() => {
                      if (file.mime.startsWith("image/")) return theme.accent
                      if (file.mime === "application/pdf") return theme.primary
                      return theme.secondary
                    })
                    return (
                      <text fg={theme.text}>
                        <span style={{ bg: bg(), fg: theme.background }}> {MIME_BADGE[file.mime] ?? file.mime} </span>
                        <span style={{ bg: theme.backgroundElement, fg: theme.textMuted }}> {file.filename} </span>
                      </text>
                    )
                  }}
                </For>
              </box>
            </Show>
            <Show
              when={queued()}
              fallback={
                <Show when={ctx.showTimestamps()}>
                  <text fg={theme.textMuted}>
                    <span style={{ fg: theme.textMuted }}>
                      {Locale.todayTimeOrDateTime(props.message.time.created)}
                    </span>
                  </text>
                </Show>
              }
            >
              <text fg={theme.textMuted}>
                <span style={{ bg: color(), fg: queuedFg(), bold: true }}> QUEUED </span>
              </text>
            </Show>
          </box>
        </box>
      </Show>
      <Show when={compaction()}>
        <box
          marginTop={1}
          border={["top"]}
          title=" Compaction "
          titleAlignment="center"
          borderColor={theme.borderActive}
        />
      </Show>
    </>
  )
}

function AssistantMessage(props: { message: AssistantMessage; parts: Part[]; last: boolean }) {
  const ctx = use()
  const local = useLocal()
  const { theme } = useTheme()
  const sync = useSync()
  const messages = createMemo(() => sync.data.message[props.message.sessionID] ?? [])
  const model = createMemo(() => Model.name(ctx.providers(), props.message.providerID, props.message.modelID))

  const final = createMemo(() => {
    return props.message.finish && !["tool-calls", "unknown"].includes(props.message.finish)
  })

  const duration = createMemo(() => {
    if (!final()) return 0
    if (!props.message.time.completed) return 0
    const user = messages().find((x) => x.role === "user" && x.id === props.message.parentID)
    if (!user || !user.time) return 0
    return props.message.time.completed - user.time.created
  })

  const rosterShortcut = useCommandShortcut("pane.roster")
  const backgroundShortcut = useCommandShortcut("session.background")

  // Subagents already rendered via task tool parts (by session id or description).
  const coveredTaskKeys = createMemo(() => {
    const keys = new Set<string>()
    const tree = ctx.runTree()
    for (const part of props.parts) {
      if (part.type !== "tool" || part.tool !== "task") continue
      const metadata =
        part.state.status === "pending" ? {} : ((part.state.metadata as Record<string, unknown> | undefined) ?? {})
      const input = (part.state.input as Record<string, unknown> | undefined) ?? {}
      const output = part.state.status === "completed" ? part.state.output : undefined
      for (const member of resolveTaskMembers({
        input,
        metadata,
        output: typeof output === "string" ? output : undefined,
        background: metadata.background === true,
      })) {
        keys.add(`desc:${member.description}`)
        const sessionID = resolveTaskSessionId(member, tree)
        if (sessionID) keys.add(`ses:${sessionID}`)
      }
    }
    return keys
  })

  // Live tree members not yet attached to a task row (e.g. mid-spawn before
  // tool input finished) still appear in the main message so status is visible.
  // Only on the latest assistant message to avoid replaying the whole roster on
  // every historical turn.
  const extraLaunched = createMemo(() => {
    if (!props.last || props.message.sessionID !== ctx.sessionID) return [] as TaskMemberView[]
    const coveredSessions = new Set(
      [...coveredTaskKeys()].filter((key) => key.startsWith("ses:")).map((key) => key.slice(4)),
    )
    return launchedMembersFromTree(ctx.runTree(), coveredSessions).filter(
      (member) => !coveredTaskKeys().has(`desc:${member.description}`),
    )
  })

  const hasTaskUi = createMemo(
    () => props.parts.some((x) => x.type === "tool" && x.tool === "task") || extraLaunched().length > 0,
  )

  return (
    <>
      <For each={props.parts}>
        {(part, index) => {
          const component = createMemo(() => PART_MAPPING[part.type as keyof typeof PART_MAPPING])
          return (
            <Show when={component()}>
              <Dynamic
                last={index() === props.parts.length - 1}
                component={component()}
                part={part as any}
                message={props.message}
              />
            </Show>
          )
        }}
      </For>
      <For each={extraLaunched()}>
        {(member) => (
          <TaskMemberRow
            member={member}
            sessionID={member.sessionId}
            part={
              {
                id: `tree-${member.sessionId ?? member.description}`,
                sessionID: props.message.sessionID,
                messageID: props.message.id,
                type: "tool",
                callID: `tree-${member.sessionId ?? member.description}`,
                tool: "task",
                state: {
                  status: member.status === "done" || member.status === "completed" ? "completed" : "running",
                  input: {
                    description: member.description,
                    subagent_type: member.subagentType,
                  },
                  ...(member.status === "done" || member.status === "completed"
                    ? { output: "", title: "", metadata: { sessionId: member.sessionId }, time: { start: 0, end: 0 } }
                    : { time: { start: 0 } }),
                },
              } as ToolPart
            }
            toolRunning={member.status === "running" || member.status === "spawning" || member.status === "busy"}
          />
        )}
      </For>
      <Show when={hasTaskUi()}>
        <box paddingTop={1} paddingLeft={3}>
          <text fg={theme.text}>
            {rosterShortcut()}
            <span style={{ fg: theme.textMuted }}> subagent roster</span>
            <Show
              when={
                sync.data.capabilities.experimentalBackgroundSubagents &&
                props.parts.some(
                  (x) =>
                    x.type === "tool" &&
                    x.tool === "task" &&
                    x.state.status === "running" &&
                    x.state.metadata?.background !== true,
                )
              }
            >
              <span style={{ fg: theme.textMuted }}> · </span>
              {backgroundShortcut()}
              <span style={{ fg: theme.textMuted }}> background</span>
            </Show>
          </text>
        </box>
      </Show>
      <Show when={props.message.error && props.message.error.name !== "MessageAbortedError"}>
        <box
          ref={(el: BoxRenderable) => alwaysSeparate.add(el)}
          border={["left"]}
          paddingTop={1}
          paddingBottom={1}
          paddingLeft={2}
          marginTop={1}
          backgroundColor={theme.backgroundPanel}
          customBorderChars={SplitBorder.customBorderChars}
          borderColor={theme.error}
        >
          <text fg={theme.textMuted}>{props.message.error?.data.message}</text>
        </box>
      </Show>
      <Switch>
        <Match when={props.last || final() || props.message.error?.name === "MessageAbortedError"}>
          <box ref={(el: BoxRenderable) => alwaysSeparate.add(el)} paddingLeft={3}>
            <text marginTop={1}>
              <span
                style={{
                  fg:
                    props.message.error?.name === "MessageAbortedError"
                      ? theme.textMuted
                      : local.agent.color(props.message.agent),
                }}
              >
                ▣{" "}
              </span>{" "}
              <span style={{ fg: theme.text }}>{Locale.titlecase(props.message.mode)}</span>
              <span style={{ fg: theme.textMuted }}> · {model()}</span>
              <Show when={duration()}>
                <span style={{ fg: theme.textMuted }}> · {Locale.duration(duration())}</span>
              </Show>
              <Show when={props.message.error?.name === "MessageAbortedError"}>
                <span style={{ fg: theme.textMuted }}> · interrupted</span>
              </Show>
            </text>
          </box>
        </Match>
      </Switch>
    </>
  )
}

const PART_MAPPING = {
  text: TextPart,
  tool: ToolPart,
  reasoning: ReasoningPart,
}

const INLINE_TOOL_ICON_WIDTH = 2

function ReasoningPart(props: { last: boolean; part: ReasoningPart; message: AssistantMessage }) {
  const { theme } = useTheme()
  const ctx = use()
  // Collapsed by default in hide mode: a single line throughout, so the
  // layout never shifts. Click to open the full markdown block, click to close.
  const [expanded, setExpanded] = createSignal(false)

  const content = createMemo(() => {
    // OpenRouter encrypts some reasoning blocks; drop the placeholder.
    return props.part.text.replace("[REDACTED]", "").trim()
  })
  // Reasoning is finalized when the server sets `time.end` (see processor.ts).
  // Flips independently of the parent message completing.
  const isDone = createMemo(() => props.part.time.end !== undefined)
  const inMinimal = createMemo(() => ctx.thinkingMode() === "hide")
  const duration = createMemo(() => {
    const end = props.part.time.end
    return end === undefined ? 0 : Math.max(0, end - props.part.time.start)
  })
  const summary = createMemo(() => reasoningSummary(content()))
  const syntax = createSyntaxStyleMemo(() => generateSubtleSyntax(theme))

  const toggle = () => {
    if (!inMinimal()) return
    setExpanded((prev) => !prev)
  }

  return (
    <Show when={content()}>
      <box
        ref={(el: BoxRenderable) => alwaysSeparate.add(el)}
        paddingLeft={3}
        marginTop={1}
        flexDirection="column"
        flexShrink={0}
      >
        <box onMouseUp={toggle}>
          <ReasoningHeader
            toggleable={inMinimal()}
            open={!inMinimal() || expanded()}
            done={isDone()}
            title={summary().title}
            duration={isDone() ? Locale.duration(duration()) : undefined}
          />
        </box>
        <Show when={(!inMinimal() || expanded()) && summary().body}>
          <box paddingLeft={inMinimal() ? 2 : 0} marginTop={1}>
            <code
              filetype="markdown"
              drawUnstyledText={false}
              streaming={true}
              syntaxStyle={syntax()}
              content={summary().body}
              conceal={ctx.conceal()}
              fg={theme.textMuted}
            />
          </box>
        </Show>
      </box>
    </Show>
  )
}

function ReasoningHeader(props: {
  toggleable: boolean
  open: boolean
  done: boolean
  title: string | null
  duration?: string
}) {
  const { theme } = useTheme()
  const fg = () =>
    props.open
      ? RGBA.fromValues(theme.warning.r, theme.warning.g, theme.warning.b, theme.thinkingOpacity)
      : theme.warning

  return (
    <Switch>
      <Match when={!props.done}>
        <box flexDirection="row">
          <Spinner color={fg()}>{props.title ? "Thinking: " + props.title : "Thinking"}</Spinner>
        </box>
      </Match>
      <Match when={true}>
        <text fg={fg()} wrapMode="none">
          <Show when={props.toggleable}>
            <span>{props.open ? "- " : "+ "}</span>
          </Show>
          <span>Thought</span>
          <Show when={props.title || props.duration}>
            <span>: </span>
          </Show>
          <Show when={props.title}>
            <span>{props.title}</span>
          </Show>
          <Show when={props.duration}>
            <span>
              {props.title ? " · " : ""}
              {props.duration}
            </span>
          </Show>
        </text>
      </Match>
    </Switch>
  )
}

function TextPart(props: { last: boolean; part: TextPart; message: AssistantMessage }) {
  const ctx = use()
  const { theme, syntax } = useTheme()
  return (
    <Show when={props.part.text.trim()}>
      <box ref={(el: BoxRenderable) => alwaysSeparate.add(el)} paddingLeft={3} marginTop={1} flexShrink={0}>
        <markdown
          syntaxStyle={syntax()}
          streaming={true}
          internalBlockMode="top-level"
          content={props.part.text.trim()}
          tableOptions={{ style: "grid" }}
          conceal={ctx.conceal()}
          fg={theme.markdownText}
          bg={theme.background}
        />
      </box>
    </Show>
  )
}

// Pending messages moved to individual tool pending functions

function ToolPart(props: { last: boolean; part: ToolPart; message: AssistantMessage }) {
  const ctx = use()
  const display = createMemo(() => toolDisplay(props.part.tool))

  // Hide tool if showDetails is false and tool completed successfully.
  // Always keep `task` rows: users need subagent status in the main message
  // (OpenCode parity), even when other tool details are collapsed.
  const shouldHide = createMemo(() => {
    if (ctx.showDetails()) return false
    if (props.part.tool === "task") return false
    if (props.part.state.status !== "completed") return false
    return true
  })

  const toolprops = {
    get metadata() {
      return props.part.state.status === "pending" ? {} : (props.part.state.metadata ?? {})
    },
    get input() {
      return props.part.state.input ?? {}
    },
    get output() {
      return props.part.state.status === "completed" ? props.part.state.output : undefined
    },
    get tool() {
      return props.part.tool
    },
    get part() {
      return props.part
    },
  }

  return (
    <Show when={!shouldHide()}>
      <Switch>
        <Match when={display() === "bash"}>
          <Shell {...toolprops} />
        </Match>
        <Match when={display() === "glob"}>
          <Glob {...toolprops} />
        </Match>
        <Match when={display() === "read"}>
          <Read {...toolprops} />
        </Match>
        <Match when={display() === "grep"}>
          <Grep {...toolprops} />
        </Match>
        <Match when={display() === "webfetch"}>
          <WebFetch {...toolprops} />
        </Match>
        <Match when={display() === "websearch"}>
          <WebSearch {...toolprops} />
        </Match>
        <Match when={display() === "write"}>
          <Write {...toolprops} />
        </Match>
        <Match when={display() === "edit"}>
          <Edit {...toolprops} />
        </Match>
        <Match when={display() === "task"}>
          <Task {...toolprops} />
        </Match>
        <Match when={display() === "apply_patch"}>
          <ApplyPatch {...toolprops} />
        </Match>
        <Match when={display() === "todowrite"}>
          <TodoWrite {...toolprops} />
        </Match>
        <Match when={display() === "question"}>
          <Question {...toolprops} />
        </Match>
        <Match when={display() === "skill"}>
          <Skill {...toolprops} />
        </Match>
        <Match when={true}>
          <GenericTool {...toolprops} />
        </Match>
      </Switch>
    </Show>
  )
}

type ToolProps = {
  input: Record<string, unknown>
  metadata: Record<string, unknown>
  tool: string
  output?: string
  part: ToolPart
}
function GenericTool(props: ToolProps) {
  const { theme } = useTheme()
  const ctx = use()
  const output = createMemo(() => props.output?.trim() ?? "")
  const [expanded, setExpanded] = createSignal(false)
  const maxLines = 3
  const maxChars = createMemo(() => maxLines * Math.max(20, ctx.width - 6))
  const collapsed = createMemo(() => collapseToolOutput(output(), maxLines, maxChars()))
  const limited = createMemo(() => {
    if (expanded() || !collapsed().overflow) return output()
    return collapsed().output
  })

  return (
    <Show
      when={props.output && ctx.showGenericToolOutput()}
      fallback={
        <InlineTool icon="⚙" pending="Writing command..." complete={true} part={props.part}>
          {props.tool} {input(props.input)}
        </InlineTool>
      }
    >
      <BlockTool
        title={`# ${props.tool} ${input(props.input)}`}
        part={props.part}
        onClick={collapsed().overflow ? () => setExpanded((prev) => !prev) : undefined}
      >
        <box gap={1}>
          <text fg={theme.text}>{limited()}</text>
          <Show when={collapsed().overflow}>
            <text fg={theme.textMuted}>{expanded() ? "Click to collapse" : "Click to expand"}</text>
          </Show>
        </box>
      </BlockTool>
    </Show>
  )
}

function InlineTool(props: {
  icon: string
  iconColor?: RGBA
  color?: RGBA
  complete: unknown
  pending: string
  failure?: string
  spinner?: boolean
  separate?: boolean
  children: JSX.Element
  part: ToolPart
  onClick?: () => void
}) {
  const { theme } = useTheme()
  const ctx = use()
  const sync = useSync()
  const renderer = useRenderer()
  const [hover, setHover] = createSignal(false)
  const [errorExpanded, setErrorExpanded] = createSignal(false)

  const permission = createMemo(() => {
    const callID = sync.data.permission[ctx.sessionID]?.at(0)?.tool?.callID
    if (!callID) return false
    return callID === props.part.callID
  })

  const error = createMemo(() => (props.part.state.status === "error" ? props.part.state.error : undefined))

  const denied = createMemo(
    () =>
      error()?.includes("QuestionRejectedError") ||
      error()?.includes("rejected permission") ||
      error()?.includes("specified a rule") ||
      error()?.includes("user dismissed"),
  )

  const failed = createMemo(() => Boolean(error() && !denied()))
  const clickable = createMemo(() => Boolean(props.onClick || failed()))
  const fg = createMemo(() => {
    if (props.color) return props.color
    if (permission()) return theme.warning
    if (failed()) return theme.error
    if (hover() && props.onClick) return theme.text
    if (props.complete) return theme.textMuted
    return theme.text
  })

  return (
    <InlineToolRow
      icon={props.icon}
      iconColor={props.iconColor}
      color={fg()}
      errorColor={theme.error}
      failed={failed()}
      denied={Boolean(denied())}
      error={error()}
      errorExpanded={errorExpanded()}
      complete={props.complete}
      pending={props.pending}
      failure={props.failure}
      spinner={props.spinner}
      separate={props.separate}
      onMouseOver={() => clickable() && setHover(true)}
      onMouseOut={() => setHover(false)}
      onMouseUp={() => {
        if (renderer.getSelection()?.getSelectedText()) return
        if (failed()) {
          setErrorExpanded((value) => !value)
          return
        }
        props.onClick?.()
      }}
    >
      {props.children}
    </InlineToolRow>
  )
}

export function InlineToolRow(props: {
  icon: string
  iconColor?: RGBA
  color?: RGBA
  errorColor?: RGBA
  failed?: boolean
  denied?: boolean
  error?: string
  errorExpanded?: boolean
  complete: unknown
  pending: string
  failure?: string
  spinner?: boolean
  separate?: boolean
  children: JSX.Element
  onMouseOver?: () => void
  onMouseOut?: () => void
  onMouseUp?: () => void
}) {
  return (
    <box
      paddingLeft={3}
      onMouseOver={props.onMouseOver}
      onMouseOut={props.onMouseOut}
      onMouseUp={props.onMouseUp}
      ref={(el: BoxRenderable) => {
        if (props.separate) alwaysSeparate.add(el)
        setPreLayoutSiblingMargin(el, (previous) => {
          return props.separate ||
            (previous instanceof BoxRenderable && (previous.height > 1 || alwaysSeparate.has(previous)))
            ? 1
            : 0
        })
      }}
    >
      <Switch>
        <Match when={props.spinner}>
          <Spinner color={props.color} children={props.children} />
        </Match>
        <Match when={true}>
          <Show
            fallback={
              <text
                paddingLeft={3}
                fg={props.color}
                attributes={props.denied ? TextAttributes.STRIKETHROUGH : undefined}
              >
                ~ {props.pending}
              </text>
            }
            when={props.complete || props.failed}
          >
            <box flexDirection="row">
              <text
                width={INLINE_TOOL_ICON_WIDTH}
                fg={props.failed ? props.errorColor : (props.iconColor ?? props.color)}
                attributes={props.denied ? TextAttributes.STRIKETHROUGH : undefined}
              >
                {props.icon}
              </text>
              <text
                flexGrow={1}
                fg={props.failed ? props.errorColor : props.color}
                attributes={props.denied ? TextAttributes.STRIKETHROUGH : undefined}
              >
                {props.failed && !props.complete ? (props.failure ?? props.children) : props.children}
              </text>
            </box>
          </Show>
        </Match>
      </Switch>
      <Show when={props.failed && props.errorExpanded}>
        <box paddingLeft={INLINE_TOOL_ICON_WIDTH}>
          <text fg={props.errorColor}>{props.error}</text>
        </box>
      </Show>
    </box>
  )
}

function BlockTool(props: {
  title: string
  children: JSX.Element
  onClick?: () => void
  part?: ToolPart
  spinner?: boolean
}) {
  const { theme } = useTheme()
  const renderer = useRenderer()
  const [hover, setHover] = createSignal(false)
  const error = createMemo(() => (props.part?.state.status === "error" ? props.part.state.error : undefined))
  return (
    <box
      ref={(el: BoxRenderable) => alwaysSeparate.add(el)}
      border={["left"]}
      paddingTop={1}
      paddingBottom={1}
      paddingLeft={2}
      marginTop={1}
      gap={1}
      backgroundColor={hover() ? theme.backgroundMenu : theme.backgroundPanel}
      customBorderChars={SplitBorder.customBorderChars}
      borderColor={theme.background}
      onMouseOver={() => props.onClick && setHover(true)}
      onMouseOut={() => setHover(false)}
      onMouseUp={() => {
        if (renderer.getSelection()?.getSelectedText()) return
        props.onClick?.()
      }}
    >
      <Show
        when={props.spinner}
        fallback={
          <text paddingLeft={3} fg={theme.textMuted}>
            {props.title}
          </text>
        }
      >
        <Spinner color={theme.textMuted}>{props.title.replace(/^# /, "")}</Spinner>
      </Show>
      {props.children}
      <Show when={error()}>
        <text fg={theme.error}>{error()}</text>
      </Show>
    </box>
  )
}

function Shell(props: ToolProps) {
  const { theme } = useTheme()
  const pathFormatter = usePathFormatter()
  const ctx = use()
  const isRunning = createMemo(() => props.part.state.status === "running")
  const output = createMemo(() => stripAnsi(stringValue(props.metadata.output)?.trim() ?? ""))
  const [expanded, setExpanded] = createSignal(false)
  const maxLines = 10
  const maxChars = createMemo(() => maxLines * Math.max(20, ctx.width - 6))
  const collapsed = createMemo(() => collapseToolOutput(output(), maxLines, maxChars()))
  const limited = createMemo(() => {
    if (expanded() || !collapsed().overflow) return output()
    return collapsed().output
  })

  const workdirDisplay = createMemo(() => {
    const workdir = stringValue(props.input.workdir)
    if (!workdir || workdir === ".") return undefined
    return pathFormatter.format(workdir)
  })

  const title = createMemo(() => {
    const desc = stringValue(props.input.description) ?? "Shell"
    const wd = workdirDisplay()
    if (!wd) return `# ${desc}`
    if (desc.includes(wd)) return `# ${desc}`
    return `# ${desc} in ${wd}`
  })

  return (
    <Switch>
      <Match when={stringValue(props.metadata.output) !== undefined}>
        <BlockTool
          title={title()}
          part={props.part}
          spinner={isRunning()}
          onClick={collapsed().overflow ? () => setExpanded((prev) => !prev) : undefined}
        >
          <box gap={1}>
            <text fg={theme.text}>$ {stringValue(props.input.command)}</text>
            <Show when={output()}>
              <text fg={theme.text}>{limited()}</text>
            </Show>
            <Show when={collapsed().overflow}>
              <text fg={theme.textMuted}>{expanded() ? "Click to collapse" : "Click to expand"}</text>
            </Show>
          </box>
        </BlockTool>
      </Match>
      <Match when={true}>
        <InlineTool icon="$" pending="Writing command..." complete={stringValue(props.input.command)} part={props.part}>
          {stringValue(props.input.command)}
        </InlineTool>
      </Match>
    </Switch>
  )
}

function Write(props: ToolProps) {
  const { theme, syntax } = useTheme()
  const pathFormatter = usePathFormatter()
  const code = createMemo(() => {
    return stringValue(props.input.content) ?? ""
  })

  return (
    <Switch>
      <Match when={props.metadata.diagnostics !== undefined}>
        <BlockTool title={"# Wrote " + pathFormatter.format(stringValue(props.input.filePath))} part={props.part}>
          <line_number fg={theme.textMuted} minWidth={3} paddingRight={1}>
            <code
              conceal={false}
              fg={theme.text}
              filetype={filetype(stringValue(props.input.filePath))}
              syntaxStyle={syntax()}
              content={code()}
            />
          </line_number>
          <Diagnostics diagnostics={props.metadata.diagnostics} filePath={stringValue(props.input.filePath) ?? ""} />
        </BlockTool>
      </Match>
      <Match when={true}>
        <InlineTool
          icon="←"
          pending="Preparing write..."
          complete={stringValue(props.input.filePath)}
          part={props.part}
        >
          Write {pathFormatter.format(stringValue(props.input.filePath))}
        </InlineTool>
      </Match>
    </Switch>
  )
}

function Glob(props: ToolProps) {
  const pathFormatter = usePathFormatter()
  return (
    <InlineTool icon="✱" pending="Finding files..." complete={stringValue(props.input.pattern)} part={props.part}>
      Glob "{stringValue(props.input.pattern)}"{" "}
      <Show when={stringValue(props.input.path)}>in {pathFormatter.format(stringValue(props.input.path))} </Show>
      <Show when={numberValue(props.metadata.count)}>
        ({numberValue(props.metadata.count)} {numberValue(props.metadata.count) === 1 ? "match" : "matches"})
      </Show>
    </InlineTool>
  )
}

function Read(props: ToolProps) {
  const { theme } = useTheme()
  const pathFormatter = usePathFormatter()
  const isRunning = createMemo(() => props.part.state.status === "running")
  const loaded = createMemo(() => {
    if (props.part.state.status !== "completed") return []
    if (props.part.state.time.compacted) return []
    const value = props.metadata.loaded
    if (!value || !Array.isArray(value)) return []
    return value.filter((p): p is string => typeof p === "string")
  })
  return (
    <>
      <InlineTool
        icon="→"
        pending="Reading file..."
        complete={stringValue(props.input.filePath)}
        spinner={isRunning()}
        part={props.part}
      >
        Read {pathFormatter.format(stringValue(props.input.filePath))} {input(props.input, ["filePath"])}
      </InlineTool>
      <For each={loaded()}>
        {(filepath) => (
          <box paddingLeft={3}>
            <text paddingLeft={3} fg={theme.textMuted}>
              ↳ Loaded {pathFormatter.format(filepath)}
            </text>
          </box>
        )}
      </For>
    </>
  )
}

function Grep(props: ToolProps) {
  const pathFormatter = usePathFormatter()
  return (
    <InlineTool icon="✱" pending="Searching content..." complete={stringValue(props.input.pattern)} part={props.part}>
      Grep "{stringValue(props.input.pattern)}"{" "}
      <Show when={stringValue(props.input.path)}>in {pathFormatter.format(stringValue(props.input.path))} </Show>
      <Show when={numberValue(props.metadata.matches)}>
        ({numberValue(props.metadata.matches)} {numberValue(props.metadata.matches) === 1 ? "match" : "matches"})
      </Show>
    </InlineTool>
  )
}

function WebFetch(props: ToolProps) {
  return (
    <InlineTool icon="%" pending="Fetching from the web..." complete={stringValue(props.input.url)} part={props.part}>
      WebFetch {stringValue(props.input.url)}
    </InlineTool>
  )
}

function WebSearch(props: ToolProps) {
  return (
    <InlineTool icon="◈" pending="Searching web..." complete={stringValue(props.input.query)} part={props.part}>
      {webSearchProviderLabel(props.metadata.provider)} "{stringValue(props.input.query)}"{" "}
      <Show when={numberValue(props.metadata.numResults)}>({numberValue(props.metadata.numResults)} results)</Show>
    </InlineTool>
  )
}

function Task(props: ToolProps) {
  const ctx = use()
  const { theme } = useTheme()
  const sync = useSync()
  const dialog = useDialog()

  const members = createMemo(() =>
    resolveTaskMembers({
      input: props.input,
      metadata: props.metadata,
      output: props.output,
      background: props.metadata.background === true,
    }),
  )

  const resolvedMembers = createMemo(() => {
    const tree = ctx.runTree()
    return members().map((member) => ({
      member,
      sessionID: resolveTaskSessionId(member, tree),
    }))
  })

  createEffect(() => {
    for (const { sessionID } of resolvedMembers()) {
      if (sessionID && !sync.data.message[sessionID]?.length) void sync.session.sync(sessionID)
    }
  })

  return (
    <Show
      when={resolvedMembers().length > 0}
      fallback={
        <InlineTool
          icon="│"
          separate={true}
          spinner={props.part.state.status === "running" || props.part.state.status === "pending"}
          complete={false}
          pending="Delegating..."
          part={props.part}
        >
          {""}
        </InlineTool>
      }
    >
      <For each={resolvedMembers()}>
        {(row) => (
          <TaskMemberRow
            member={row.member}
            sessionID={row.sessionID}
            part={props.part}
            toolRunning={props.part.state.status === "running"}
          />
        )}
      </For>
    </Show>
  )
}

function TaskMemberRow(props: {
  member: TaskMemberView
  sessionID?: string
  part: ToolPart
  toolRunning: boolean
}) {
  const ctx = use()
  const { theme } = useTheme()
  const sync = useSync()
  const dialog = useDialog()

  const messages = createMemo(() => sync.data.message[props.sessionID ?? ""] ?? [])

  const tools = createMemo(() => {
    return messages().flatMap((msg) =>
      (sync.data.part[msg.id] ?? [])
        .filter((part): part is ToolPart => part.type === "tool")
        .map((part) => ({ tool: part.tool, state: part.state })),
    )
  })

  const current = createMemo(() =>
    tools().findLast((x) => (x.state.status === "running" || x.state.status === "completed") && x.state.title),
  )

  const status = createMemo(() => sync.data.session_status[props.sessionID ?? ""])
  const isRunning = createMemo(() => {
    const value = status()
    const memberStatus = props.member.status
    if (memberStatus === "done" || memberStatus === "completed" || memberStatus === "failed" || memberStatus === "cancelled") {
      return false
    }
    return (
      props.toolRunning ||
      memberStatus === "running" ||
      memberStatus === "spawning" ||
      memberStatus === "busy" ||
      (props.member.background && value !== undefined && value.type !== "idle")
    )
  })
  const retry = createMemo(() => {
    const value = status()
    if (value?.type !== "retry") return
    return value
  })
  const failed = createMemo(() => {
    const memberStatus = props.member.status
    return (
      props.part.state.status === "error" ||
      memberStatus === "failed" ||
      memberStatus === "error" ||
      !!retry()
    )
  })

  const duration = createMemo(() => {
    const first = messages().find((x) => x.role === "user")?.time.created
    const assistant = messages().findLast((x) => x.role === "assistant")?.time.completed
    if (!first || !assistant) return 0
    return assistant - first
  })

  const content = createMemo(() => {
    if (!props.member.description) return ""
    const lines = [
      formatSubagentTitle(
        Locale.titlecase(props.member.subagentType || "General"),
        props.member.description,
        props.member.background,
      ),
    ]

    const retrying = retry()
    if (isRunning() && retrying) {
      lines.push(`↳ ${formatSubagentRetry(retrying.attempt, Locale.truncate(retrying.message, 80))}`)
    } else if (isRunning() && tools().length > 0) {
      if (current()) {
        const state = current()!.state
        const title = state.status === "running" || state.status === "completed" ? state.title : undefined
        lines.push(`↳ ${Locale.titlecase(current()!.tool)} ${title ?? ""}`.trimEnd())
      } else lines.push(`↳ ${formatSubagentToolcalls(tools().length)}`)
    } else if (isRunning()) {
      lines.push("↳ Working...")
    }

    if (!isRunning() && (props.part.state.status === "completed" || props.member.status === "done" || props.member.status === "completed")) {
      if (props.member.summary) {
        lines.push(`↳ ${Locale.truncate(props.member.summary, 120)}`)
      } else {
        lines.push(`↳ ${formatCompletedSubagentDetail(tools().length, Locale.duration(duration()))}`)
      }
    }

    if (failed() && props.member.summary) {
      lines.push(`↳ ${Locale.truncate(props.member.summary, 120)}`)
    }

    return lines.join("\n")
  })

  return (
    <InlineTool
      icon={failed() ? "✗" : !isRunning() && props.part.state.status !== "pending" ? "✓" : "│"}
      separate={true}
      color={failed() ? theme.error : undefined}
      spinner={isRunning()}
      complete={props.member.description}
      pending="Delegating..."
      part={props.part}
      onClick={() => {
        if (props.sessionID) ctx.openSubagent(props.sessionID)
        const status = retry()
        if (status) void DialogAlert.show(dialog, "Retry Error", status.message)
      }}
    >
      {content()}
    </InlineTool>
  )
}

export function formatSubagentToolcalls(count: number) {
  return `${count} toolcall${count === 1 ? "" : "s"}`
}

export function formatSubagentTitle(agent: string, description: string, background: boolean) {
  return `${agent} Task${background ? " (background)" : ""} — ${description}`
}

export function formatSubagentRetry(attempt: number, message: string) {
  return `Retrying (attempt ${attempt}) · ${message}`
}

export function formatCompletedSubagentDetail(toolcalls: number, duration: string) {
  if (toolcalls === 0) return duration
  return `${formatSubagentToolcalls(toolcalls)} · ${duration}`
}

function Edit(props: ToolProps) {
  const ctx = use()
  const { theme, syntax } = useTheme()
  const pathFormatter = usePathFormatter()

  const view = createMemo(() => {
    const diffStyle = ctx.tui.diff_style
    if (diffStyle === "stacked") return "unified"
    // Default to "auto" behavior
    return ctx.width > 120 ? "split" : "unified"
  })

  const ft = createMemo(() => filetype(stringValue(props.input.filePath)))

  const diffContent = createMemo(() => stringValue(props.metadata.diff) ?? "")

  return (
    <Switch>
      <Match when={stringValue(props.metadata.diff) !== undefined}>
        <BlockTool title={"← Edit " + pathFormatter.format(stringValue(props.input.filePath))} part={props.part}>
          <box paddingLeft={1}>
            <diff
              diff={diffContent()}
              view={view()}
              filetype={ft()}
              syntaxStyle={syntax()}
              showLineNumbers={true}
              width="100%"
              wrapMode={ctx.diffWrapMode()}
              fg={theme.text}
              addedBg={theme.diffAddedBg}
              removedBg={theme.diffRemovedBg}
              contextBg={theme.diffContextBg}
              addedSignColor={theme.diffHighlightAdded}
              removedSignColor={theme.diffHighlightRemoved}
              lineNumberFg={theme.diffLineNumber}
              lineNumberBg={theme.diffContextBg}
              addedLineNumberBg={theme.diffAddedLineNumberBg}
              removedLineNumberBg={theme.diffRemovedLineNumberBg}
            />
          </box>
          <Diagnostics diagnostics={props.metadata.diagnostics} filePath={stringValue(props.input.filePath) ?? ""} />
        </BlockTool>
      </Match>
      <Match when={true}>
        <InlineTool icon="←" pending="Preparing edit..." complete={stringValue(props.input.filePath)} part={props.part}>
          Edit {pathFormatter.format(stringValue(props.input.filePath))} {input({ replaceAll: props.input.replaceAll })}
        </InlineTool>
      </Match>
    </Switch>
  )
}

function ApplyPatch(props: ToolProps) {
  const ctx = use()
  const { theme, syntax } = useTheme()
  const pathFormatter = usePathFormatter()

  const files = createMemo(() => parseApplyPatchFiles(props.metadata.files))

  const view = createMemo(() => {
    const diffStyle = ctx.tui.diff_style
    if (diffStyle === "stacked") return "unified"
    return ctx.width > 120 ? "split" : "unified"
  })

  function Diff(p: { diff: string; filePath: string }) {
    return (
      <box paddingLeft={1}>
        <diff
          diff={p.diff}
          view={view()}
          filetype={filetype(p.filePath)}
          syntaxStyle={syntax()}
          showLineNumbers={true}
          width="100%"
          wrapMode={ctx.diffWrapMode()}
          fg={theme.text}
          addedBg={theme.diffAddedBg}
          removedBg={theme.diffRemovedBg}
          contextBg={theme.diffContextBg}
          addedSignColor={theme.diffHighlightAdded}
          removedSignColor={theme.diffHighlightRemoved}
          lineNumberFg={theme.diffLineNumber}
          lineNumberBg={theme.diffContextBg}
          addedLineNumberBg={theme.diffAddedLineNumberBg}
          removedLineNumberBg={theme.diffRemovedLineNumberBg}
        />
      </box>
    )
  }

  function title(file: { type: string; relativePath: string; filePath: string; deletions: number }) {
    if (file.type === "delete") return "# Deleted " + file.relativePath
    if (file.type === "add") return "# Created " + file.relativePath
    if (file.type === "move") return "# Moved " + pathFormatter.format(file.filePath) + " → " + file.relativePath
    return "← Patched " + file.relativePath
  }

  return (
    <Switch>
      <Match when={files().length > 0}>
        <For each={files()}>
          {(file) => (
            <BlockTool title={title(file)} part={props.part}>
              <Show
                when={file.type !== "delete"}
                fallback={
                  <text fg={theme.diffRemoved}>
                    -{file.deletions} line{file.deletions !== 1 ? "s" : ""}
                  </text>
                }
              >
                <Diff diff={file.patch} filePath={file.filePath} />
                <Diagnostics diagnostics={props.metadata.diagnostics} filePath={file.movePath ?? file.filePath} />
              </Show>
            </BlockTool>
          )}
        </For>
      </Match>
      <Match when={true}>
        <InlineTool icon="%" pending="Preparing patch..." failure="Patch failed" complete={false} part={props.part}>
          Patch
        </InlineTool>
      </Match>
    </Switch>
  )
}

function TodoWrite(props: ToolProps) {
  const todos = createMemo(() => parseTodos(props.input.todos))
  return (
    <Switch>
      <Match when={parseTodos(props.metadata.todos).length}>
        <BlockTool title="# Todos" part={props.part}>
          <box>
            <For each={todos()}>{(todo) => <TodoItem status={todo.status} content={todo.content} />}</For>
          </box>
        </BlockTool>
      </Match>
      <Match when={true}>
        <InlineTool
          icon="⚙"
          pending="Updating todos..."
          failure="Todo update failed"
          complete={false}
          part={props.part}
        >
          Updating todos...
        </InlineTool>
      </Match>
    </Switch>
  )
}

function Question(props: ToolProps) {
  const { theme } = useTheme()
  const questions = createMemo(() => parseQuestions(props.input.questions))
  const answers = createMemo(() => parseQuestionAnswers(props.metadata.answers))
  const count = createMemo(() => questions().length)

  function format(answer?: ReadonlyArray<string>) {
    if (!answer?.length) return "(no answer)"
    return answer.join(", ")
  }

  return (
    <Switch>
      <Match when={answers()}>
        <BlockTool title="# Questions" part={props.part}>
          <box gap={1}>
            <For each={questions()}>
              {(q, i) => (
                <box flexDirection="column">
                  <text fg={theme.textMuted}>{q.question}</text>
                  <text fg={theme.text}>{format(answers()?.[i()])}</text>
                </box>
              )}
            </For>
          </box>
        </BlockTool>
      </Match>
      <Match when={true}>
        <InlineTool icon="→" pending="Asking questions..." complete={count()} part={props.part}>
          Asked {count()} question{count() !== 1 ? "s" : ""}
        </InlineTool>
      </Match>
    </Switch>
  )
}

function Skill(props: ToolProps) {
  return (
    <InlineTool icon="→" pending="Loading skill..." complete={stringValue(props.input.name)} part={props.part}>
      Skill "{stringValue(props.input.name)}"
    </InlineTool>
  )
}

function Diagnostics(props: { diagnostics: unknown; filePath: string }) {
  const { theme } = useTheme()
  const terminalEnvironment = useTuiTerminalEnvironment()
  const errors = createMemo(() => {
    const normalized = normalizePath(
      typeof props.filePath === "string" ? props.filePath : "",
      terminalEnvironment.platform,
    )
    return parseDiagnostics(props.diagnostics, normalized)
  })

  return (
    <Show when={errors().length}>
      <box>
        <For each={errors()}>
          {(diagnostic) => (
            <text fg={theme.error}>
              Error [{diagnostic.range.start.line + 1}:{diagnostic.range.start.character + 1}] {diagnostic.message}
            </text>
          )}
        </For>
      </box>
    </Show>
  )
}

function input(input: Record<string, unknown>, omit?: string[]): string {
  const primitives = Object.entries(input).filter(([key, value]) => {
    if (omit?.includes(key)) return false
    return typeof value === "string" || typeof value === "number" || typeof value === "boolean"
  })
  if (primitives.length === 0) return ""
  return `[${primitives.map(([key, value]) => `${key}=${value}`).join(", ")}]`
}

function stringValue(value: unknown) {
  return typeof value === "string" ? value : undefined
}

function numberValue(value: unknown) {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}

const toolDisplays = new Set([
  "bash",
  "glob",
  "read",
  "grep",
  "webfetch",
  "websearch",
  "write",
  "edit",
  "task",
  "apply_patch",
  "todowrite",
  "question",
  "skill",
])

export function toolDisplay(tool: string) {
  return toolDisplays.has(tool) ? tool : "generic"
}

function recordValue(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return
  return value as Record<string, unknown>
}

export function parseApplyPatchFiles(value: unknown) {
  if (!Array.isArray(value)) return []
  return value.flatMap((item) => {
    const file = recordValue(item)
    if (!file) return []
    const type = stringValue(file.type)
    const relativePath = stringValue(file.relativePath)
    const filePath = stringValue(file.filePath)
    const patch = stringValue(file.patch)
    const deletions = numberValue(file.deletions)
    if (!type || !relativePath || !filePath || patch === undefined || deletions === undefined) return []
    return [{ type, relativePath, filePath, patch, deletions, movePath: stringValue(file.movePath) }]
  })
}

export function parseTodos(value: unknown) {
  if (!Array.isArray(value)) return []
  return value.flatMap((item) => {
    const todo = recordValue(item)
    const status = stringValue(todo?.status)
    const content = stringValue(todo?.content)
    return status && content ? [{ status, content }] : []
  })
}

export function parseQuestions(value: unknown) {
  if (!Array.isArray(value)) return []
  return value.flatMap((item) => {
    const question = stringValue(recordValue(item)?.question)
    return question ? [{ question }] : []
  })
}

export function parseQuestionAnswers(value: unknown) {
  if (!Array.isArray(value)) return
  return value.map((answer) =>
    Array.isArray(answer) ? answer.filter((item): item is string => typeof item === "string") : [],
  )
}

export function parseDiagnostics(value: unknown, filePath: string) {
  const diagnostics = recordValue(value)?.[filePath]
  if (!Array.isArray(diagnostics)) return []
  return diagnostics
    .flatMap((item) => {
      const diagnostic = recordValue(item)
      const start = recordValue(recordValue(diagnostic?.range)?.start)
      const line = numberValue(start?.line)
      const character = numberValue(start?.character)
      const message = stringValue(diagnostic?.message)
      if (diagnostic?.severity !== 1 || line === undefined || character === undefined || !message) return []
      return [{ range: { start: { line, character } }, message }]
    })
    .slice(0, 3)
}
