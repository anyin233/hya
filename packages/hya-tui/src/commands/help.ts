/** Text of the `/help` view. Keep in step with the registry in native.ts. */
export const helpText = [
  "Type a prompt to create a session (if needed) and start a turn.",
  "Other /commands are sent to the backend command catalog.",
  "", "/new [agent] [model]   Create a session", "/sessions             Refresh session list", "/open <id|number>     Open a session",
  "/models               List models", "/model <provider/model> Change current session model", "/workflows            List workflows",
  "/keys                 List saved provider key names", "/key set <provider>   Enter a key in a concealed prompt",
  "/login <provider>     Alias for /key set", "/key remove <provider> Delete a saved key",
  "/workflow select <name> | /workflow run [name]", "/interactions         Show pending permissions and questions",
  "/approve <id> | /deny <id> | /answer <id> <text>", "/cancel               Cancel current turn", "/refresh              Refresh all views",
  "/sidebar [on|off]     Show or hide the sidebar (Ctrl+B)", "/thinking [on|off]    Expand or collapse reasoning (Ctrl+O)",
  "/api                  List all v1 HTTP operations", "/api METHOD /v1/path [JSON object]", "",
  "Tab completes commands · Ctrl+R refresh · Ctrl+B sidebar · Ctrl+O thinking",
  "PgUp/PgDn scroll · Ctrl+Home/Ctrl+End top/bottom (Home/End when the input is empty) · Ctrl+C quit",
].join("\n")
