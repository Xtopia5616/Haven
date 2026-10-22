# Haven third-party licenses and acknowledgments

Haven-authored source code is licensed under the MIT License in [`LICENSE`](LICENSE). That grant does not replace the terms for third-party code, fonts, models, or other materials listed here. The full resolved Rust and JavaScript dependency inventory is in [`docs/dependency-license-inventory.md`](docs/dependency-license-inventory.md).

## Bundled materials

| Material | Where it is used | Copyright / license | Source |
|---|---|---|---|
| Silero VAD v5 model | `assets/models/silero_vad.onnx`, embedded by `crates/input/src/vad.rs` | Copyright 2020-present Silero Team; MIT | [Silero VAD](https://github.com/snakers4/silero-vad); license copy: [`assets/models/LICENSE-Silero-VAD.txt`](assets/models/LICENSE-Silero-VAD.txt) |
| Roboto font files | `ui/static/fonts/roboto/` | Copyright 2011 The Roboto Project Authors; SIL Open Font License 1.1 | [Google Fonts](https://github.com/google/fonts/tree/main/ofl/roboto) |
| JetBrains Mono font files | `ui/static/fonts/jetbrains-mono/` | Copyright 2020 The JetBrains Mono Project Authors; SIL Open Font License 1.1 | [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) |
| Lucide and Feather-derived icon geometry | `ui/src/lib/icons.ts` | Lucide Icons and Contributors; ISC. Feather-derived icons: Cole Bemis; MIT. | [Lucide](https://github.com/lucide-icons/lucide); [Feather](https://github.com/feathericons/feather) |

The OFL text and both font copyright notices are included in [`ui/static/fonts/OFL.txt`](ui/static/fonts/OFL.txt), which is copied into the built UI. The Silero model's MIT text is next to the model asset. The icon registry comment points here to preserve its attribution at the source location.

## Architecture and design references

- **Everything and Everything CLI (`es.exe`)** — optional user-installed software used by `files.search` when explicitly enabled in local `config.toml`; Haven does not bundle or modify either binary. The [Everything application license](https://www.voidtools.com/License.txt) grants use and redistribution under MIT terms and includes a separate PCRE notice. The [voidtools/ES repository](https://github.com/voidtools/ES) declares MIT and includes its [license text](https://github.com/voidtools/ES/blob/main/LICENSE). Haven's integration calls the documented [ES command-line interface](https://www.voidtools.com/support/everything/command_line_interface/) and does not redistribute either binary.

- **Pi Coding Agent** — acknowledged as an architectural reference for the ReAct agent loop. Haven's agent integration and source implementation are maintained in this repository; the audit did not identify copied Pi source files. See [Pi](https://github.com/badlogic/pi-mono).
- **Google Material Design 3 Expressive** — visual and interaction design reference for Haven's design tokens and components. Haven's UI implementation is maintained in this repository; Material Design's guidance is not bundled as source code or artwork. See [Material Design 3](https://m3.material.io/).
- **Agent Skills specification** — Haven parses the public `SKILL.md` manifest format and follows its directory conventions. Haven's parser and runner are maintained here; the specification is not source code. See [Agent Skills](https://agentskills.io/specification).
- **Model Context Protocol (MCP)** — Haven implements the public protocol for connecting to MCP servers. Haven's transport and protocol code are maintained here; no MCP SDK is bundled. See the [MCP specification](https://modelcontextprotocol.io/specification).
- **Model provider APIs** — provider adapters and locally maintained fixtures follow public provider API documentation. The current provider documentation URLs are maintained with the presets in [`ui/src/lib/providerPresets.ts`](ui/src/lib/providerPresets.ts); no provider SDK source is bundled as a separate dependency.

The repository's Haven branding (mark, generated icons, and renderer) is maintained in `assets/branding/`, `scripts/generate-icons.py`, and `ui/src/lib/`; the provenance record is [ADR 0072](docs/adr/0072-haven-brand-icon-system.md).

## Lucide and Feather license texts

The icon paths include Lucide geometry; Lucide's license identifies a set of icons derived from Feather and supplies both applicable license notices. This combined notice is reproduced below.

### Lucide — ISC License

Copyright (c) 2026 Lucide Icons and Contributors

Permission to use, copy, modify, and/or distribute this software for any purpose with or without fee is hereby granted, provided that the above copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.

### Feather-derived icons — MIT License

Copyright (c) 2013-present Cole Bemis

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## Dependency inventory

The generated inventory lists exact versions and the license expressions declared by each locked package. It covers direct, transitive, development, build, and target-specific dependencies resolved by Cargo and pnpm. Each package retains its own license; the Haven MIT License does not relicense dependencies.

The dependency graph is not all MIT/Apache. Examples of other declared terms include MPL-2.0 (`cssparser`, `selectors`, `lightningcss`), Unicode-3.0 (ICU and Unicode crates), CDLA-Permissive-2.0 (`webpki-root-certs`), BSL-1.0 (`clipboard-win`, `error-code`), and PSF-2.0 (`argparse`). The inventory preserves each package's complete declared expression, including `OR` and `AND` terms; consult the linked upstream package record for its full license and notices.
