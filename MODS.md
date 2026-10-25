# emailOO

emailOO is part of the SuitOO app suite. It is an unofficial fork of [Flectar Mail](https://github.com/flectar/mail) by
[imr7997rmi](https://github.com/imr7997rmi). It tracks upstream closely and adds
a few improvements on top. Not affiliated with or endorsed by Flectar; the
Flectar name and logo belong to their owners (see [TRADEMARKS.md](TRADEMARKS.md)).
Licensed under the same terms as upstream, AGPL-3.0-only. emailOO is derived from Flectar Mail and is not produced, sponsored, or supported by Flectar.

## Branches

| Branch | Purpose |
|---|---|
| `main` | Mirror of `flectar/mail` `main`, no changes |
| `mods/*` | One improvement per branch, rebased on `main` |
| `mejoras` | All `mods/*` merged — the branch that is built and used (default) |

## Improvements

| Branch | What it does | Issue |
|---|---|---|
| `mods/drag-drop-attachments` | Drop files from Finder or a file manager on the window to attach them while composing | #1 |
| `mods/reduce-motion` | "Reduce motion" switch in Settings; every animation, including hover colors, becomes instant | — |
| `mods/full-config-backup` | Settings export/import also carries signatures, default signatures and manual sender identities | #5 |
| `mods/macos-icon-margin` | macOS icon with Apple's standard transparent margin, so it is not larger than other Dock icons | #7 |
| `mods/about-version` | About shows the mod label and the upstream base it was built on | — |
| `mods/ai-assistant` | AI in Settings (OpenAI-compatible endpoint, model, key in keychain) with user presets; AI menu in the reader and composer, replacing the selection in drafts | #9 |
| `mods/hide-services` | Hides Flectar's paid Services page | #10 |
| `mods/icloud-sync` | Optional settings sync through iCloud Drive/emailOO (no credentials) | #11 |
| `mods/build-scripts` | `scripts/mod-install-macos.sh`: packages and installs *emailOO.app* | — |

Planned: configurable shortcuts (#2), message in a new window (#3), Nextcloud
share links instead of large attachments (#4), macOS app menu tweaks (#6),
full emailOO branding inside the app (#8).

## Following upstream

```bash
git fetch upstream
git switch main && git merge --ff-only upstream/main && git push origin main
for b in $(git for-each-ref --format='%(refname:short)' refs/heads/mods/); do git rebase main "$b"; done
git switch mejoras && git reset --hard main && git merge --no-edit $(git for-each-ref --format='%(refname:short)' refs/heads/mods/)
git push --force-with-lease origin 'mods/*' mejoras
```

## Building on macOS

```bash
export CARGO_TARGET_DIR=~/.cache/flectar-mail-target
export FLECTAR_MOD_LABEL="imr7997rmi mod · base $(git describe --tags main)"
# Fast development build (dependencies optimized, app code incremental)
cargo build --locked --config 'profile.dev.package."*".opt-level=2' --bin flectar-mail
scripts/mod-install-macos.sh debug
# Optimized build
cargo build --release --locked && scripts/mod-install-macos.sh release
```

The app installs as *emailOO* (`com.nivvoo.emailoo`) and uses the
same data directory as Flectar Mail. Opening it upgrades the mail database, so
an older Flectar Mail release cannot open that data afterwards — back it up
first. Create a code-signing certificate named `emailOO Local` in Keychain
Access to keep keychain access across rebuilds.
