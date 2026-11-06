# Prepared model and reasoning selection

This capability is dormant by default. No browser selector is currently shown, and client overrides have no effect while `allow_model_selection` is false. Install the updated backend and native client once. Later activation only needs Admin configuration and an updated published browser UI, with no further binary rebuild.

Admin → AI and context → Future client model selection stores the enable switch and the `available_models` allowlist. Both are included in settings backups. Enabling selection requires an explicit backend default model included in the allowlist, and any configured default reasoning level must be allowed for it. Example catalog (replace the example model with one supported by the installed provider runtime):

```json
[{"id":"your-model-id","reasoning_levels":["low","medium","high"]}]
```

The startup manifest at `/v1/client-config`, forwarded to the browser as `brand.model_catalog` in `/api/bootstrap` and live state, contains `enabled`, `defaults` (`model`, `reasoning_effort`), and `models`. Restart native clients to fetch changed settings, as with branding. Catalog publication does not guarantee an account has quota or access to a model. Reasoning levels must also be supported by the installed App Server.

A future browser UI sends this optional object with `/api/chat`; the native client forwards it with `/v1/chat`:

```json
{"generation":{"model":"your-model-id","reasoning_effort":"high"}}
```

Omit `generation` (or send `{}`) to use backend defaults. If no effort is supplied, the backend uses its configured reasoning level for the default model, otherwise the first listed reasoning level for the chosen model (or no effort for an empty list). A reasoning-only override uses the backend's configured model. Enabled selection rejects any model or reasoning level outside the allowlist before credit admission. Disabled selection records but ignores the requested preferences. Token billing continues to use the existing global rates; this does not introduce model-specific pricing.

Each queued prompt owns its options snapshot. Queue edits must resend the `generation` object returned by the edit endpoint; previews include it too. Retries retain it. Steering changes the running task's instructions and never switches its model or reasoning level.

`request_generation` stores the requested and effective JSON, owned by `(user_id, request_id)`, atomically with task admission. Effective settings are reused after reconnects, backend restarts and account failover, even if the operator changes defaults or disables selection. Actual provider model/token counters remain in `usage`; these small settings records contain no prompt or attachments.

Terminal mode is also prepared: after enabling selection, `/model` lists the catalog, `/model MODEL [REASONING]` selects preferences for subsequent prompts, and `/model default` returns to backend defaults. No new terminal controls are shown while disabled.
