import type { TuiPlugin, TuiPluginApi } from "@opencode-ai/plugin/tui"
import { createMemo, Show } from "solid-js"

import { presentWorkflow } from "../../../hya/workflow-presentation"
import type { BuiltinTuiPlugin } from "../builtins"

const id = "internal:sidebar-workflow"

/** Read hya's Workflow extension from the synchronized upstream Session value. */
function workflowValue(session: unknown): unknown {
  if (!session || typeof session !== "object" || Array.isArray(session)) return undefined
  return (session as Record<string, unknown>).workflow
}

/** Read bounded Workflow-scoped activity from the synchronized Session value. */
function memberValues(session: unknown): unknown {
  if (!session || typeof session !== "object" || Array.isArray(session)) return []
  return (session as Record<string, unknown>).workflowActivity ?? []
}

/** Render the read-only Workflow summary for one owning Session. */
function View(props: { api: TuiPluginApi; session_id: string }) {
  const theme = () => props.api.theme.current
  const state = createMemo(() => {
    const session = props.api.state.session.get(props.session_id)
    return presentWorkflow(workflowValue(session), memberValues(session))
  })
  const summary = createMemo(() =>
    state().state === "none" || state().state === "invalid" ? state().state : `${state().name} · ${state().state}`,
  )
  const color = createMemo(() => {
    switch (state().tone) {
      case "info":
        return theme().info
      case "success":
        return theme().success
      case "warning":
        return theme().warning
      case "error":
        return theme().error
      case "muted":
        return theme().textMuted
    }
  })

  return (
    <box>
      <text fg={theme().text}>
        <b>Workflow</b>
      </text>
      <text fg={color()}>{summary()}</text>
      <Show when={state().revision} keyed>
        {(revision) => <text fg={theme().textMuted}>revision {revision}</text>}
      </Show>
      <Show when={state().agentProgress && state().stageProgress}>
        <text fg={theme().textMuted}>
          {state().agentProgress} · {state().stageProgress}
        </text>
      </Show>
      <Show when={state().levelProgress} keyed>
        {(level) => <text fg={theme().textMuted}>{level}</text>}
      </Show>
      <Show when={state().activeStages} keyed>
        {(stages) => <text fg={theme().text}>active {stages}</text>}
      </Show>
      <Show when={state().currentWork} keyed>
        {(work) => <text fg={theme().textMuted}>{work}</text>}
      </Show>
    </box>
  )
}

/** Register the hya Workflow view ahead of the stock sidebar sections. */
const tui: TuiPlugin = async (api) => {
  api.slots.register({
    order: 50,
    slots: {
      /** Render Workflow state for the Session owning this sidebar. */
      sidebar_content(_ctx, props) {
        return <View api={api} session_id={props.session_id} />
      },
    },
  })
}

const plugin: BuiltinTuiPlugin = {
  id,
  tui,
}

export default plugin
