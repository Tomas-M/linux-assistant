# Third-party source

The client embeds the [Font Awesome Free paperclip icon](https://fontawesome.com/icons/paperclip?f=classic&s=solid), Copyright 2026 Fonticons, Inc., licensed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Its attribution is preserved in the embedded SVG source.

Sign-in buttons embed Google's [official Google Identity logo](https://developers.google.com/identity/branding-guidelines), Microsoft's four-color brand mark, and GitHub's [Octicons mark-github](https://github.com/primer/octicons) (MIT). Provider marks remain the property of their respective owners and identify the corresponding sign-in service.

The backend embeds the Codex App Server from [openai/codex](https://github.com/openai/codex), licensed under Apache License 2.0. The exact source revision is recorded by the `vendor/codex` Git submodule; the release name is in the root Cargo manifest. Upstream files are unmodified.

The complete upstream [LICENSE](vendor/codex/LICENSE) and [NOTICE](vendor/codex/NOTICE) are included in every backend executable and can be displayed with `assistant-server --third-party-notices`. Preserve those notices when redistributing it. Other Rust dependencies retain their own licenses, recorded by their package manifests in the dependency graph.

Application integration changes are in `crates/server/src/embedded.rs` and `crates/server/src/provider/`. See the README for the source update and verification workflow.
