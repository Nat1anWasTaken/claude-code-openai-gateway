# Claude Code OpenAI Gateway

Tiny Rust HTTP server that forwards OpenAI-style `POST /v1/chat/completions` requests to the Claude CLI (`claude --output-format stream-json`) and streams the results back as either SSE (`stream: true`) or a single JSON completion.

## Quick start

1. Install Rust and make sure `claude` is on `PATH`.
2. Build:

   ```bash
   cargo build --release
   ```

3. Run:

   ```bash
   ./target/release/claude-code-openai-gateway
   ```

4. Send OpenAI-compatible requests to `http://localhost:8080/v1/chat/completions`.

## Docker

The published image does **not** bundle the Claude CLI. Mount your local `claude` binary into the container and run:

```bash
docker run --rm -p 8080:8080 \
  -v /full/path/to/claude:/usr/local/bin/claude:ro \
  ghcr.io/<owner>/<repo>:latest
```
The image sets `/usr/local/bin/package.json` to `"type": "module"` so that ESM-based `claude` binaries execute without module warnings.

Build locally instead:

```bash
docker build -t ghcr.io/<owner>/<repo>:local .
```

The Dockerfile uses the Rust nightly toolchain to support the 2024 edition; this is encapsulated inside the image and does not affect your host toolchain.

Note: the runtime image now installs `nodejs` so that Node-based `claude` CLI builds work when you mount the binary.

### Mounting credentials

Where the CLI stores credentials (as of Nov 20, 2025):
- **Linux:** file-based at `~/.claude/credentials.json` (preferred), sometimes `~/.claude/.credentials.json`, and occasionally `~/.config/claude/credentials.json` (older builds). citeturn0search3turn0search2
- **macOS:** stored in the Keychain under service `Claude Code-credentials` (recent builds) or `Claude Code` (some affected versions). Keys are not on disk by default. Export with:  
  `security find-generic-password -s "Claude Code-credentials" -w > ~/.claude/credentials.json`  
  If that item doesn’t exist, try `Claude Code`. You can also run `claude /login` inside the container once to generate file-based creds. citeturn0search0turn0reddit14

Mount the binary, credential files, and set `HOME` so the gateway can reuse the login:

```bash
docker run --rm -p 8080:8080 \
  -e HOME=/home/app \
  -v /full/path/to/claude:/usr/local/bin/claude:ro \
  -v ~/.claude:/home/app/.claude \
  -v ~/.config/claude:/home/app/.config/claude \
  ghcr.io/<owner>/<repo>:latest
```

If your creds are only in the macOS keychain and you don’t want to export them, log in once inside the container to generate file-based creds:

```bash
docker run -it --rm \
  -e HOME=/home/app \
  -v /full/path/to/claude:/usr/local/bin/claude:ro \
  -v ~/.claude:/home/app/.claude \
  -v ~/.config/claude:/home/app/.config/claude \
  ghcr.io/<owner>/<repo>:latest \
  claude /login
```

Tip: Some builds also expect `~/.claude/.credentials.json` (note the leading dot in the filename) and `~/.claude.json` for API key fallback; mount those too if they exist on your host.

## What you get

- **Streaming mode:** SSE chunks mirroring OpenAI deltas while the Claude CLI is running.
- **Non-streaming mode:** A single OpenAI-style completion result, including any usage metadata.
- **Session caching:** Conversation hashes map to Claude session IDs so repeated histories can resume with `--resume`.

Errors from Claude or parsing issues are returned as HTTP 500 responses.

## License

MIT; see [LICENSE.txt](LICENSE.txt) for full terms.
