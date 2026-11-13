# BioCompass Harness

BioCompass Harness (`dsh`) is an open-source, local-first scientific agent harness built by [Alithea Bio](https://github.com/AlitheaBio/BioCompass-harness) on the [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) **everything-is-a-plugin** architecture and powered by [Cordis](https://github.com/cordiverse/cordis) ([_A Programming Paradigm for Spatiotemporal Composability_](https://arxiv.org/abs/2608.25512)).

All cloud telemetry, session-log uploaders, proprietary Office conversion binaries, and unprompted registry pings have been stripped so the Web UI, Electron Desktop app, headless CLI, TypeScript SDK, and Python SDK run cleanly with zero external phone-home dependencies.

Review the [safety notice](SAFETY.md) before running the project.

## Run from source

Install Node.js (`>=24.9.0`) and `pnpm`, then build and launch the Web UI:

```sh
git clone https://github.com/AlitheaBio/BioCompass-harness.git
cd BioCompass-harness
pnpm install
pnpm run build
pnpm dsh web
```

`pnpm run build` prepares the repository artifacts. `pnpm dsh web` starts the Web UI at `http://127.0.0.1:3080` by default and opens it in the browser (`--no-open` skips opening a browser). See the [Web UI guide](docs/user/guide/index.md).

### Model providers and API keys

Model access runs through `packages/llm/llm-pi-ai` (`anthropic`, `openai`, `google`, `deepseek`, and `openrouter`), defaulting to `anthropic` (`claude-sonnet-4-5`). Configure credentials in **Settings → Models** or via environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`) and optional `*_BASE_URL` overrides (`ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL`, `GEMINI_BASE_URL`, `DEEPSEEK_BASE_URL`, `OPENROUTER_BASE_URL`).

### Other surfaces

- **Web development watch mode:** `pnpm run dev:web` builds artifacts, serves the Web UI, and rebuilds client bundles on source edits.
- **Desktop app:** `pnpm run dev:desktop` builds and launches the Electron Desktop app.
- **Headless CLI:** `pnpm dsh --profile headless "task"` runs one task from source.
- **SDKs:** TypeScript (`packages/sdk`) and Python (`python/sdk` + `python/sdk-runtime`) drive the same `dsh` profiles over JSON-RPC.

## Development

Start with the [development guide](docs/development.md), [architecture documentation](docs/architecture.md), and [package map](packages/README.md).

For agents, follow [AGENTS.md](AGENTS.md).

## Upstream attribution

Derived from [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) by DeepSeek AI:

```bibtex
@misc{deepseek-harness2026,
  title={DeepSeek Harness: Everything is a Plugin},
  author={DeepSeek-AI},
  year={2026},
  publisher={GitHub},
  howpublished={\url{https://github.com/deepseek-ai/deepseek-harness}},
}
```

## License

[MIT](LICENSE)

Third-party dependencies and their licenses are disclosed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
