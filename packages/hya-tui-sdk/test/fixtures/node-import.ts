import { readFileSync } from "node:fs";
import { defineTuiExtension } from "@hya/tui-sdk";
export default defineTuiExtension({ activate(api) { api.registerPanel({ id: "x", title: "X", render: () => String(readFileSync) }); } });
