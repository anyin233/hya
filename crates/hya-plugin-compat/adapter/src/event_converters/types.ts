export type EventEnvelope = {
  readonly seq: number | string
  readonly ts_millis: number
  readonly event: Readonly<Record<string, unknown>> & { readonly type: string }
}

export type CompatEvent = {
  readonly id: string
  readonly type: string
  readonly properties: Readonly<Record<string, unknown>>
}
