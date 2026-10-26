# How does provider login copy a URL to the local desktop over SSH?

The ChatGPT login form owns unmodified F2 and dispatches `ProviderCommand` `{kind: "copyLoginUrl"}`. `app/providers.ts` receives the same `copyText(text): boolean` terminal transport used by MCP; the app controller delegates to OpenTUI OSC 52. Copy the original OAuth authorization URL, never its wrapped display or the concealed callback value. Render feedback inside the active form, without duplicating it behind the form. No backend clipboard endpoint or remote xclip/wl-copy is involved.

## Does explicit JSX hyperlink markup guarantee Windows Terminal Ctrl+click?

No. The form now uses `<a href={authorizationUrl}>`, and render tests verify the complete link target. OpenTUI 0.5.12 gates OSC 8 output on native terminal capabilities. Its `isHyperlinkTerm` does not include MicrosoftTerminal/Windows Terminal; WT_SESSION enables hyperlinks for native Windows or WSL, not general Linux SSH. Do not claim that markup alone fixes this case. Use F2 while the upstream capability detector lacks support. See https://github.com/anomalyco/opentui/blob/v0.5.12/packages/native/src/terminal.zig .

Shared provider specs capture real OSC 52 writes over both HTTP and gRPC, decode the complete URL, and verify a narrow resize does not insert display wrapping. Controller coverage checks copy failure, modified keys, and callback secrecy/preservation. Clipboard success means that the transport sent the write; OSC 52 has no acknowledgement that the terminal accepted it.
