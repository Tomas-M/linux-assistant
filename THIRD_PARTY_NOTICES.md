# Third-party source

The client embeds the unmodified browser build of [markdown-it 15.0.3](https://www.npmjs.com/package/markdown-it/v/15.0.3), Copyright (c) 2014 Vitaly Puzrin, Alex Kocharin, under the MIT license. Its complete license notice is included in `crates/client/web/markdown.js`, which is embedded in the executable alongside the parser. HTML input and remote images are disabled.

The client and admin embed unmodified icons from [Font Awesome Free 7.3.1](https://github.com/FortAwesome/Font-Awesome/tree/7.3.1/svgs), Copyright 2026 Fonticons, Inc., under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). The shared SVG sprite in `crates/protocol/assets/icons.svg` preserves attribution and identifies each source icon.

Sign-in buttons retain Google's [official color logo](https://developers.google.com/identity/branding-guidelines), Microsoft's four-color mark, and GitHub's [Octicons mark-github](https://github.com/primer/octicons) (MIT). Provider marks remain the property of their respective owners.

The backend embeds the Codex App Server from [openai/codex](https://github.com/openai/codex), licensed under Apache License 2.0. The exact source revision is recorded by the `vendor/codex` Git submodule; the release name is in the root Cargo manifest. Upstream files are unmodified.

The complete upstream [LICENSE](vendor/codex/LICENSE) and [NOTICE](vendor/codex/NOTICE) are included in every backend executable and can be displayed with `assistant-server --third-party-notices`. Preserve those notices when redistributing it. Other Rust dependencies retain their own licenses, recorded by their package manifests in the dependency graph.

Application integration changes are in `crates/server/src/embedded.rs` and `crates/server/src/provider/`. See the README for the source update and verification workflow.
