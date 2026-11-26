# Troubleshooting

Quick fixes for the most common issues when installing, starting, or connecting Hya.

---

## Startup & UI issues

### Running `hya` prints a banner and exits instead of opening the UI

Bare `hya` opens the TUI and WebUI only when both `stdin` and `stdout` are real interactive terminals. If you ran `hya` inside a script, cron job, or pipe (`echo hi | hya`), use **`hya exec "..."`** instead.

### `WebUI unavailable: port 3250 is in use`

Another process (often a second `hya` window!) is already using port `3250`. Your terminal TUI still works fine. To get a WebUI for this window too, pick another port or let Hya pick a free one:

```sh
hya --port 3251
hya --port 0       # Picks a free port; check /status to see the URL
```

### `Bun is required...` or `the TUI is not installed`

- If you copied only the `hya` binary out of `~/.local/bin` to another folder without `~/.local/lib/hya` and `~/.local/bundles`, reinstall cleanly with `hya update --force` (or re-run the `curl ... | sh` installer).
- If you are running from a Git source checkout (`./target/debug/hya`), install frontend dependencies once first:
  ```sh
  (cd packages/hya-tui && bun install --frozen-lockfile)
  (cd packages/hya-tui-web && bun install --frozen-lockfile)
  ```

### Where do I find Hya's crash or error logs?

Check these two log files:

```sh
# TUI launcher & WebUI host log
tail -n 50 ~/.local/state/hya/hya.log

# Background server (daemon) log
tail -n 50 ~/.local/state/hya/sessions.db.server.log
```

---

## Background server & database lock issues

### `database ... is already in use` (Exit code `75`)

Only one Hya server can own a `.db` file at a time (`<db>.runtime-owner.lock`).

- **Starting a second `hya serve` on the default DB?** You don't need to—just run `hya` and it attaches to the running daemon automatically, or pass a separate `--db ./test.db`.
- **Running `hya --db ... exec --bare` or `--pure` while a daemon owns that DB?** A running daemon already loaded its full config, so `--bare` and `--pure` cannot piggyback on it. Omit `--db` (to run in memory), pass a different `--db`, or stop the daemon first with `hya serve stop`.
- **Daemon hung during startup?** Force-restart it:
  ```sh
  hya serve stop --force
  hya serve start
  ```
  *(Never delete `<db>.runtime-owner.lock` by hand—the OS kernel releases the lock automatically as soon as the process exits.)*

### `backend 0.x ≠ tui 0.y · hya serve restart`

You updated `hya`, and the old background server is still running from before the update. Restart it—any open TUI windows will reconnect automatically:

```sh
hya serve restart
```

### `Backend stopped (hya serve stop) · /reconnect starts it again`

Someone ran `hya serve stop` in another terminal. Type **`/reconnect`** inside the TUI (or run `hya serve start`) to bring the server back up.

---

## Model & Provider issues

### Hya is stuck on `hya/offline` and just echoes my messages

No live model was found! Check:
1. Open **`/key`** in the TUI (or run `hya provider list`) to see whether any provider is configured and marked `ready`.
2. If you edited `~/.config/hya/config.yaml` by hand, watch out for typos like `provider:` instead of **`providers:`** (plural!), then run `hya serve restart`.
3. If you used `api_key: "{env:ANTHROPIC_API_KEY}"`, verify `echo "$ANTHROPIC_API_KEY"` is non-empty in the shell *before* starting the daemon (or save the key permanently with `hya login anthropic "$ANTHROPIC_API_KEY"`).

### `unknown provider for model`

The model ID you asked for isn't listed under any configured provider. Run `hya models --refresh` to pull the latest remote model list, then check `hya models` for the exact `provider/model` spelling.

### Edits or shell commands immediately fail in `hya exec`, `-p`, or `hya loop`

In non-interactive headless runs, there is no human at the keyboard to answer `"Allow this command?"` prompts—so any `"ask"` prompt is **denied automatically** for safety.

To let headless scripts edit files or run specific commands:
- Add explicit `allow` rules under `permission.rules` in `~/.config/hya/config.yaml`, **or**
- Pass `--yolo` *only* when running inside an isolated container or disposable environment.

---

## Network, LAN & Remote Relay issues

### `403 permission_denied: Host "..." is not an allowed name`

To block malicious websites from attacking your local Hya server through your browser (DNS rebinding), Hya accepts only `localhost`, `127.0.0.1`, and `[::1]` by default—even if you bound `0.0.0.0:8080`.

If you intentionally reach Hya over a LAN IP, Tailscale hostname, or reverse proxy, allow that hostname explicitly:

```sh
hya serve --bind 0.0.0.0:8080 --allow-host 192.168.1.20 --allow-host my-box.ts.net
# Or on the background daemon:
hya serve restart --allow-host my-box.ts.net
```

### `503 unavailable: remote backend is offline, or the relay link was rotated or is wrong`

Your laptop reached `hya proxy`, but couldn't open a room to your workstation:
1. On your workstation, check `hya serve relay status`. If disconnected, run `hya serve relay connect <proxy-url>`.
2. Did you run `hya serve relay rotate`? Old links look identical to an offline machine so strangers can't probe rooms. Grab the fresh link on your workstation with `hya serve relay link`.
3. Putting nginx, Caddy, or Cloudflare Tunnel in front of `hya proxy`? Run diagnostics to check whether HTTP/2 or WebSockets are being stripped:
   ```sh
   hya relay doctor https://relay.example.com
   ```
   If your reverse proxy strips gRPC trailers, pass `--transport ws` (WebSocket) and it will work right through standard HTTPS proxies.

---

## WSL 2 specific issues (Windows)

- **Everything feels slow:** Your repo is under `/mnt/c/...` on the Windows filesystem. Move it into your Linux home folder (`~/code/...`) for 10x faster file reads and searches.
- **Copying text in the TUI doesn't reach the Windows clipboard:** Use **Windows Terminal** (the old `conhost.exe` window doesn't support OSC 52 clipboard copy), or open `http://localhost:3250` in your Windows browser.
