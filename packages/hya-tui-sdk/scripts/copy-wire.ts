const source = new URL("../src/wire.ts", import.meta.url)
const destination = new URL("../../hya-tui/src/extensions/wire.generated.ts", import.meta.url)

await Bun.write(destination, Bun.file(source))
