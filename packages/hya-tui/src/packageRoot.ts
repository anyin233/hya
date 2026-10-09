import { resolve } from "node:path"

// src/ and dist/ both sit immediately under the package root. Keep resources
// rooted here so a compiled app still uses the shipped SDK/proto/confine files.
export const packageRoot = resolve(import.meta.dir, "..")
