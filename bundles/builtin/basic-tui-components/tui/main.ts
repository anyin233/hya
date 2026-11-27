import { defineTuiExtension, type TuiExtensionApi } from "@hya/tui-sdk"
import { registerSessions } from "./sessions"
import { registerTodos } from "./todos"
import { registerProjects } from "./projects"
import { registerContext } from "./context"
import { registerProjectView } from "./projectView"

export default defineTuiExtension({
  activate(api: TuiExtensionApi): void {
    registerSessions(api)
    registerTodos(api)
    registerProjects(api)
    registerContext(api)
    registerProjectView(api)
  },
})
