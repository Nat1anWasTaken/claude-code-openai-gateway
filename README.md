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

Build locally instead:

```bash
docker build -t ghcr.io/<owner>/<repo>:local .
```

## What you get

- **Streaming mode:** SSE chunks mirroring OpenAI deltas while the Claude CLI is running.
- **Non-streaming mode:** A single OpenAI-style completion result, including any usage metadata.
- **Session caching:** Conversation hashes map to Claude session IDs so repeated histories can resume with `--resume`.

Errors from Claude or parsing issues are returned as HTTP 500 responses.

## License

MIT; see [LICENSE.txt](LICENSE.txt) for full terms.
