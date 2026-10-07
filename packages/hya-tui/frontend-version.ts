/** The independent frontend release version. */
export const frontendVersion = "0.44.14"

/** The oldest backend release this frontend can use. */
export const minimumBackendVersion = "0.45.3"

type ReleaseVersion = readonly [number, number, number]

function parseReleaseVersion(version: string): ReleaseVersion | undefined {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:[-+].*)?$/.exec(version)
  if (!match) return undefined
  return [Number(match[1]), Number(match[2]), Number(match[3])]
}

function compareReleaseVersions(left: ReleaseVersion, right: ReleaseVersion): number {
  for (let index = 0; index < left.length; index++) {
    if (left[index] !== right[index]) return left[index] - right[index]
  }
  return 0
}

/** Return whether a backend version satisfies this frontend's minimum. */
export function isBackendVersionSupported(version: string): boolean {
  const actual = parseReleaseVersion(version)
  const minimum = parseReleaseVersion(minimumBackendVersion)
  return actual !== undefined && minimum !== undefined && compareReleaseVersions(actual, minimum) >= 0
}
