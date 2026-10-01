import { expect, test } from "bun:test"
import { frontendVersion, minimumBackendVersion, isBackendVersionSupported } from "../frontend-version"
import { backendVersionError } from "../src/version"

test("frontend compatibility accepts only backends at or above its declared minimum", () => {
  expect(frontendVersion).toBe("0.44.1")
  expect(minimumBackendVersion).toBe("0.43.41")
  expect(isBackendVersionSupported("0.43.41")).toBe(true)
  expect(isBackendVersionSupported("0.43.42")).toBe(true)
  expect(isBackendVersionSupported("0.43.40")).toBe(false)
  expect(isBackendVersionSupported("not-a-version")).toBe(false)
})

test("frontend reports incompatible backend versions explicitly", () => {
  expect(backendVersionError("0.43.40")).toBe("backend 0.43.40 is incompatible with frontend 0.44.1; requires backend >= 0.43.41")
  expect(backendVersionError("0.43.41")).toBeUndefined()
  expect(backendVersionError("")).toContain("backend unknown")
})
