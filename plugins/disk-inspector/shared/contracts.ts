/** The plugin's implemented discovery contract, independent of Hya internals. */
export const BUNDLE_ID = "hya-extra/disk-inspector";
export const PLUGIN_ID = "disk-inspector";
export const PANEL_ID = "disk";
export const CONTRACT_VERSION = 1;

export interface InspectorInfo {
  readonly contractVersion: 1;
  readonly machine: { readonly hostname: string; readonly platform: string };
  readonly capabilities: {
    readonly volumes: false;
    readonly scans: false;
    readonly cancel: false;
  };
}

/** Plugin-local seam. No assumption that the current TUI SDK implements it. */
export interface InspectorClient {
  info(): Promise<InspectorInfo>;
}

export type InspectorState =
  | { readonly kind: "disconnected" }
  | { readonly kind: "loading" }
  | { readonly kind: "failed"; readonly message: string }
  | { readonly kind: "ready"; readonly info: InspectorInfo };
