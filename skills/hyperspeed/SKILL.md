---
name: hyperspeed
description: Run and manage local dev servers, background jobs, Cloudflare tunnels and iOS simulators with the Hyperspeed CLI on macOS. Use this skill whenever the user mentions Hyperspeed or `hyperspeed`, or wants to register, start, stop, restart or check a dev server, read a server's logs, debug why a local server crashed, share localhost through a public URL, or boot, shut down or stream logs from a simulator, even if they don't name Hyperspeed, as long as it's installed.
---

# Hyperspeed

Hyperspeed supervises dev servers from a background daemon, so they outlive the terminal and the dashboard window. Prefer it over `npm run dev &` or `nohup` when it's installed: the user can then see and control the same processes in the app.

Check with `command -v hyperspeed`. If it's missing, offer to install it by running `./scripts/bundle.sh` from a clone of the repo (needs Rust).

Don't run `hyperspeed` with no arguments unless the user wants the window; it opens the GUI.

## Servers and jobs

Register from the project's directory, in the shell the project normally runs in. `add` saves that shell's `PATH`, which is how nvm, pyenv and similar tools keep resolving correctly later.

```bash
hyperspeed add web --port 3000 --health / -- npm run dev
hyperspeed add worker -- npm run jobs
hyperspeed add api --cwd ../api --env LOG_LEVEL=debug -- 'npm run build && npm start'
```

| Option | Meaning |
|---|---|
| `--port N` | Exported as `$PORT`. Omit it and Hyperspeed detects the listening port once the process binds one. |
| `--cwd DIR` | Working directory, default current. |
| `--health /path` | HTTP path probed every 5s for response time. Use a cheap route, since a dev server may recompile on each hit. |
| `--env K=V` | Repeatable. Saved in plain text in `jobs.json`, so leave secrets in the project's own `.env`. |

Everything after `--` runs through the user's shell. Quote the command if it contains `&&`, pipes or redirects, or the outer shell will consume them.

Running `add` on an existing name updates it, and the change applies on the next start, so follow it with `hyperspeed restart <name>` if the job is running.

```bash
hyperspeed ls
hyperspeed start web          # or --all
hyperspeed stop web           # or --all
hyperspeed restart web        # or --all
hyperspeed rm web             # must be stopped first
```

`start` returns as soon as the process spawns, not when the server is ready. To confirm it came up, check `ls` and read the logs. States are `running`, `stopping`, `stopped`, `exited` (code 0) and `crashed`.

Use these commands rather than `kill`. A process killed from outside is reported as crashed, and only `stop` shuts down the whole process group.

## Logs

```bash
hyperspeed logs web -n 50     # last 50 lines for one job
hyperspeed logs -n 100        # all jobs
```

`-f` follows forever, so only use it in a background task or under a timeout. Logs are held in memory (the newest 20,000 lines across all jobs) and are lost when the daemon restarts.

When a server has crashed, start with `hyperspeed logs <name> -n 100`. Hyperspeed's own lines say which signal or exit code ended it and warn if the port was already taken.

## Tunnels

A tunnel makes a local server reachable by anyone with the URL, and dev servers often expose debug routes and unauthenticated data. Confirm with the user before opening one, unless they asked for it in this conversation.

```bash
hyperspeed tunnel web                         # quick trycloudflare.com URL, prints it when live
hyperspeed tunnel web --host dev.example.com  # the user's own domain, remembered for next time
hyperspeed tunnel web --quick                 # go back to a quick URL
hyperspeed untunnel web
```

The job needs a known port and `cloudflared` must be installed (`brew install cloudflared`). `--host` also needs one prior `cloudflared tunnel login`, which the user has to do. A custom domain with an existing DNS record is left alone rather than overwritten.

`restart` keeps the tunnel and `stop` closes it. Requests reach the server with `Host: localhost:<port>`, so apps that build absolute URLs from the Host header will generate localhost links.

## Simulators

```bash
hyperspeed sim ls
hyperspeed sim boot "iPhone 17 Pro"   # name or UDID
hyperspeed sim stop <name>
hyperspeed sim restart <name>
hyperspeed sim open <name>            # show in Simulator.app
hyperspeed sim logs <name>            # toggles streaming on or off
hyperspeed logs "sim:iPhone 17 Pro" -n 50
```

A name shared across runtimes resolves to the booted one, then the newest runtime, so use the UDID when the exact device matters. `boot` returns immediately, so check `sim ls` until it says `Booted`.

A booted simulator may be in use by the user or another agent. Check with them before stopping or restarting one you didn't boot.

`sim logs` only captures apps installed on that simulator, which keeps system noise out of the shared log buffer.

If boots hang or a device is stuck in `Shutting Down`, the cause is usually CoreSimulator rather than Hyperspeed, and simctl calls give up after 3 minutes. This fixes it but closes every open simulator, so ask first:

```bash
killall -9 com.apple.CoreSimulator.CoreSimulatorService
```

## Shutting down

`hyperspeed quit` stops every managed server and closes every tunnel. Only run it when the user wants that, or after upgrading the binary so the next launch uses the new daemon. Booted simulators are left running.
