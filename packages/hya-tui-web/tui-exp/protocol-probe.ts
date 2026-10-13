// A real raw-mode terminal child that exposes received bytes as printable hex.
process.stdin.setRawMode(true)
process.stdout.write("\x1b[?1h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004hready\r\n")
let received = ""
process.stdin.on("data", (bytes: Buffer) => {
  received += bytes.toString("hex")
  process.stdout.write(`\x1b[2;1H\x1b[2Kinput:${received}`)
  if (bytes.includes(3)) process.exit(0)
})
