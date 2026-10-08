// Match the source JSX plugin's Solid client modules without loading Babel.
// Keep the resolved server.js identity: OpenTUI and the app must share one owner.
import { plugin } from "bun"
import { readFile } from "node:fs/promises"
plugin({
  name: "hya-solid-client",
  setup(build) {
    build.onLoad({ filter: /[/\\]node_modules[/\\]solid-js[/\\](?:store[/\\])?dist[/\\]server\.js$/ }, async ({ path }) => ({
      contents: await readFile(path.replace("server.js", path.includes("/store/") ? "store.js" : "solid.js"), "utf8"),
      loader: "js",
    }))
  },
})
