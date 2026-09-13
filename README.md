# factorio-save-backup-manager-rust

Factorio Save Backup Manager — Rust + Slint edition.

A desktop app to keep your Factorio saves safe: back them up to Google Drive,
get notified on Discord, and restore/link backups with a couple of clicks.

## Features

- **Google Drive integration** — OAuth2 REST uploads/downloads of save files
  (`credentials.json` + `gdrive-token.json`, same flow as the JS version).
- **Discord integration** — webhook notifications when a backup completes and
  bot-assisted downloads of the latest backup from a channel.
- **Link resolvers** — paste a share link and the app resolves it to a
  downloadable file.
- **Local saves browser** — lists your Factorio saves directory, generates test
  saves, and restores from backups.
- **Config persistence** — settings saved to `config.json` next to the
  executable (Discord bot token stored obfuscated).
- **Bilingual UI** — English and Spanish (i18n catalogs embedded at compile
  time).
- **Custom Slint UI** — themed widgets, tooltips and icons.

## Building

Requires a stable Rust toolchain (edition 2024).

```bash
cargo build --release
```

The binary is written to `target/release/factorio-save-backup-manager-rust`
(`.exe` on Windows).

## Setup

The app loads its runtime files from the directory next to the executable:

| File                | Purpose                                      | Required |
| ------------------- | -------------------------------------------- | -------- |
| `credentials.json`  | Google OAuth2 client credentials ("Desktop app" type from Google Cloud Console) | For Drive uploads |
| `gdrive-token.json` | OAuth token cache (created after first login) | Auto-generated |
| `config.json`       | App settings (created by the settings screen) | Auto-generated |

> **Note:** `credentials.json`, `gdrive-token.json` and `config.json` are
> personal/local and are intentionally **not** committed to this repository
> (see `.gitignore`).

## Tech stack

- [Rust](https://www.rust-lang.org/) (edition 2024)
- [Slint](https://slint.dev/) UI toolkit (1.17)
- `reqwest` (blocking, native-tls) for HTTP
- `serde` / `serde_json` for config and API payloads
- `chrono`, `copypasta`, `base64`, `md5`, `dirs`
