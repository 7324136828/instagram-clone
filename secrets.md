# Secrets Audit

This file records potential secrets detected during repository
productionalization.

No secret values are stored in this report.

Audit date: 2026-11-16
Scope: the full working tree (`backend/`, `frontend/`, `e2e/`, `windows-app/`,
`.github/workflows/`, scripts and documentation), excluding `.git`,
`node_modules` and build output.

Method: pattern scan for credential-shaped values — API keys, access tokens,
OAuth client secrets, cloud access keys (`AKIA...`), GitHub tokens (`ghp_`,
`github_pat_`), OpenAI/Anthropic-style keys (`sk-...`), Slack tokens
(`xox...`), Google API keys (`AIza...`), private key blocks
(`-----BEGIN ... PRIVATE KEY-----`), passwords and connection strings assigned
in source, plus a check for committed `.env`, `.pem`, `.key`, `.p12`, `.pfx`
and `id_rsa` files.

## Findings

| File | Line | Secret Type | Action | Status |
|---|---:|---|---|---|
| — | — | — | None found: no credentials, tokens, private keys, connection strings or committed `.env` files | Clean |

## Credential hygiene in place

- `.env` is excluded through `.gitignore`; only `.env.example` is tracked and
  it contains non-sensitive placeholders (port and host defaults) only.
- The CI workflow (`build-release.yml`) uses `persist-credentials: false` on
  checkout, passes the GitHub token through `${{ github.token }}` only for the
  release step, and never prints environment variables.
- The application stores no remote credentials: its only state is local
  (`settings.json` under `%APPDATA%`, the counter database and the
  `%TEMP%\instagram_cache` library).

If a real credential is ever committed, mask it, record the location here,
mark it for rotation, and check the Git history — masking a file does not make
an exposed credential safe again.
