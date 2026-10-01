# agent notes

read this before changing anything in this repo, whichever agent you are. it holds the documentation style shared by the z89 repos (lean, gloss, ember, auris, somnus and dotfiles), the decisions already settled here and any work left uncommitted, so a later change keeps the earlier work instead of undoing it.

## 🧭 before you edit

- run `git status` and `git log -3` first. uncommitted changes you did not make belong to the owner or to another session. leave them in place, and never stash, reset, check out, reformat or commit them as part of an unrelated task.
- the published branch was rewritten and force-pushed on 2026-10-01. a clone or branch from before then carries the old history. rebase its own commits onto the remote branch, never merge the old history back in and never force-push from it.
- stage the paths you changed by name, never `git add -A` or `git add .`.
- keep documentation changes and code changes in separate commits.
- commit messages are `changelog:`, a blank line, then one to four short bullets with no full stop.
- when a change touches anything this file describes, update this file in the same commit.

## ✍️ documentation style

this covers every markdown file (readme, changelog and docs). code blocks, inline code, URLs and literal values are exempt.

- the readme opens with the title in a centred `h1` and the badge row straight after it, with nothing in between. one or two neutral paragraphs then say what the project is and what it does. no pitch and no opinion words such as best, cheap, robust or seamless.
- badges are static shields.io badges with `style=flat-square&labelColor=1b1a20` in the repo colour. tech and platform badges come first and the license badge is always last, linked to `LICENSE` with `alt="license MIT"`. no stars or last-commit badges. a version on a badge is the version actually tested.
- sections run in this order, skipping any that do not apply. highlights, install, usage, settings, requirements, troubleshooting, tests, layout or architecture, docs, license. headings are lowercase with one emoji.
- highlights follow `- 🌙 **bold phrase** flows straight into the sentence.` and never `**label**: text`.
- prose has no em dashes, en dashes, double hyphens, colons or semicolons. use a comma, a full stop, parentheses, or "to" for a range. a line that would end in a colon before a list or code block ends in a full stop or is reworded. colons stay only in code, URLs, clock times and literal values.
- no filler, hedging or summary phrases ("it's worth noting", "importantly", "in summary").
- write DankMaterialShell in full once per document, followed by (DMS), then DMS from there on. the badge label is `DMS`.
- tool names are lowercase in prose (rust, cargo, bluez, pipewire, wireplumber, linux, systemd, qt, quickshell, hyprland, kitty, zsh, lua). acronyms stay uppercase (ANC, CLI, JSON, MPRIS, BLE, MIT). Apple names keep Apple's casing (AirPods, Mac, macOS, iPhone).
- units take no space (`800ms`, `5s`, `2000K`).
- `LICENSE` is the standard MIT text with `Copyright (c) 2026 z89`, byte-identical across the repos and with no trailing blank line. the readme ends with `## 📄 license` and the word MIT.

before committing a doc, `grep -nP '—|–| -- |;' <file>` should print nothing outside code, and every line `grep -n ':' <file>` prints should be code, a URL, a table of literal values or a clock time.

## 📌 settled in this repo

- `plugin.json` has `"requires_dms": ">=1.6.0"`. that was checked against the DMS v1.6.0 source (DankCommon, DankPopoutStandalone and the Theme properties auris uses), not by running auris on 1.6.
- the release tag `v0.1.0` is on ef716ac. everything after it sits under the unreleased heading in CHANGELOG. tag the commit that gives that heading a version.
- multi-host handoff, rename, in-ear pause and resume and auto-connect on case open are verified on AirPods 4 (ANC), the last three confirmed by the owner on 2026-10-01. microphone side and the listening-mode cycle stay unconfirmed. the readme, `docs/FEATURE_COMPARISON.md` and `docs/FEATURE_ROADMAP.md` must agree on every status.
- an earlier claim that auto-connect was captured working against a Mac was removed because nothing records it. add it back only with a capture.
- LibrePods links point to `librepods-org/librepods`, and quoted LibrePods text keeps its original punctuation.
- research docs state findings as "the conclusion is that", never `conclusion:`.
- the feature matrix legend is lowercase, and badges are rust and aap-l2cap, then the license, all in `8fd3ff`.

## 🚧 uncommitted work as of 2026-10-01

- `AurisWidget.qml` changes the socket redial so it keys on `root.socketStreaming` and drops a stale connection before redialing, which covers a daemon restart that rebinds the socket. it is not part of the docs work. test it against a daemon restart, then commit it on its own.
