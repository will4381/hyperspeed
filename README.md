# Hyperspeed

All your dev servers and iOS simulators in one macOS dashboard: register servers from the CLI, then start, end, restart, watch metrics and logs, and publish through a Cloudflare tunnel.

```bash
./scripts/bundle.sh                # installs the CLI and /Applications/Hyperspeed.app
```

```bash
cd ~/my-project
hyperspeed add web --port 3000 --health / -- npm run dev
hyperspeed add worker -- npm run jobs
hyperspeed start --all
hyperspeed                         # open the dashboard
hyperspeed tunnel web              # quick trycloudflare URL, printed when live
hyperspeed tunnel web --host dev.example.com   # your domain (needs `cloudflared tunnel login` once)
hyperspeed sim ls
hyperspeed sim boot "iPhone 17 Pro"
hyperspeed help
```

**Keys:** `1` `2` `3` `4` views, `↑↓` select, `S` start, `X` end, `R` restart, `T` tunnel (press twice), `↵` console, `/` filter, `-` `+` poll rate. On the Simulators tab, `S` boots, `X` shuts down, `O` opens, `L` streams app logs and `D` deletes (press twice).

Servers run under a background daemon, so closing the window leaves them up. `hyperspeed quit` stops everything. After rebuilding, run `hyperspeed quit` so the next launch uses the new daemon.

Data lives in `~/Library/Application Support/Hyperspeed/`.

**Agent skill:** [skills/hyperspeed](skills/hyperspeed/SKILL.md) teaches coding agents to use the CLI. To install it for Claude Code:

```bash
ln -s "$PWD/skills/hyperspeed" ~/.claude/skills/hyperspeed
```

**Mono font:** Relative Mono, copied from `relative-design/fonts/relative-mono` into `ui/fonts/`.
**Icons:** copied from `relative-design/dist/svg` into `ui/icons/`; add a name to `ICONS` in `ui/app.js` to use another.
