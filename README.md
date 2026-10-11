# Ardoise Son (soundcraft fork)

Web audio workstation of the Ardoise suite, based on SoundCraft by the ArtCraft Team (MIT OR Apache-2.0).
Branch `ardoise` starts from upstream tag v0.5.0 (https://github.com/storytold/soundcraft).

Changes in this fork: postMessage bridge for iframe embedding (apps/soundcraft-web/src/ardoise_bridge.rs; `open` imports
audio, MIDI and video files, there is no native single-file project to emit as `saved` in the web build), Ardoise
branding in the UI (the upstream name, wordmark and logos are not used), single web-only CI workflow
(.github/workflows/web.yml).

Build the web bundle: `cd apps/soundcraft-web && trunk build --release` (needs the wasm32-unknown-unknown target and
trunk), or `packaging/web/package.sh` to also zip it.

Licences: see LICENSE-APACHE, LICENSE-MIT and NOTICE. The copyright notices of the original authors are kept.
