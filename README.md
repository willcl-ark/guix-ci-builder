# Guix CI builder

A Rust service that listens for GitHub pull request webhooks, builds using `contrib/guix/guix-build`, and comments results (including guix hashes) back on the PR as a comment.

## Features
- Handles PR `opened` and `synchronize` events
- Cancels queued/running builds on new pushes to the same PR
- Posts results to the PR in a single comment, updating the same comment on new pushes

## Dependencies
### All
- Network access to fetch sources and post PR comments

### Nix
- A flake is provided

### Non-Nix
- `git`, `bash`, `coreutils`, and `guix` (to run the repo’s `contrib/guix/guix-build`)

## Configuration
Required:
- `GITHUB_TOKEN` — PAT with `repo` scope (to post PR comments)
- `WEBHOOK_SECRET` — shared secret for webhook signature verification

Optional:
- `GB_PORT` — server port (default: `8000`)
- `GB_HOST` — bind address (default: `0.0.0.0`)
- `WORKSPACE_PATH` — directory for checkouts/worktrees/logs (default: `/tmp/workspace`)
- `SOURCES_PATH`, `BASE_CACHE`, `SDK_PATH` — optional Guix cache paths
- `RUST_LOG` — e.g., `info` or `debug,rocket=info`

## Running
### With Nix
1) Export env vars
1) `nix run`

### Without Nix
1) Install Rust (https://rustup.rs) and system deps
2) Export env vars
3) `cargo run --release`

## GitHub Webhook Setup
- Payload URL: `http(s)://<host>:<PORT>/webhook`
- Content type: `application/json`
- Secret: `WEBHOOK_SECRET`
- Events: enable “Pull requests” (opened, synchronize)

## Endpoints
- `GET /health`
- `POST /webhook`

## CI
GitHub Actions runs fmt, clippy, tests, and docs on PRs and pushes to `main` (see `.github/workflows/check.yml`).

## Deployment note
This service responds only on specific routes (e.g., `/webhook`, `/health`).
For unknown paths you may prefer to drop connections entirely at a reverse proxy or firewall (e.g., Nginx `return 444;`).
Configure your proxy to forward only these routes to the service and handle everything else upstream.

## License
MIT
