# Grooviq

Grooviq is a free, open-source desktop DAW for beatmakers. GPL-3.0-or-later.
The owner and coding agents work in parallel threads and worktrees.

| Path             | Owns                                                                  |
| ---------------- | --------------------------------------------------------------------- |
| `dsp`            | `grooviq-dsp`: instruments and effects, allocation-free.              |
| `engine`         | `grooviq-engine`: real-time core, output devices, live-state stream.  |
| `project`        | `grooviq-project`: the project file format, saving and recovery.      |
| `tests/golden`   | `grooviq-golden`: offline reference renders compared byte for byte.   |
| `app/src-tauri`  | `grooviq-app`: the Tauri v2 desktop shell.                            |
| `app/ui`         | React + TypeScript interface.                                         |

Read the code; it is the documentation. `make help` lists the supported commands.

## Work boundaries

- Non-trivial work needs a GitHub issue. One PR per issue.
- File a GitHub issue (milestone + labels) right away for anything useful a session finds —
  a bug, a follow-up, a research result worth acting on, a deferred review finding.
  Chat, ledgers and untracked notes are not a record.
- Never play sound through the owner's speakers in automated checks.
  Tests render offline or use fake devices. Launch the app for QA only with `make qa-app`:
  no audio device (`GROOVIQ_AUDIO=null`) and a throwaway home folder.
- Audio thread: no allocation, locks, I/O or logging.
- The export path stays platform-identical: `libm` maths, no `mul_add`.
  The acceptance golden must match byte for byte on macOS and Windows.
- Built-in devices are original work. No other products' names, presets, sounds or artwork.
  Simple utility devices get plain descriptive names, such as "Multiband Compressor".
- The owner's sample kit under `/Users/yurii/Music` never enters the repo, a fixture or a test.
- A repeatable command gets a Makefile target. One-off scripts stay untracked.

## Local installs and storage

- Quality and speed is the sole local install coordinator. Other sessions report meaningful feature
  merges; Quality coalesces them and installs only verified merged `origin/main`.
- The owner authorizes local reinstall separately from deployment. Never launch the installed app,
  interrupt a running instance, or touch owner projects or samples during installation.
- Reuse one stable release cache with bounded compiler jobs. Keep the prior app until the staged copy
  is verified; record source SHA, build time and executable hash outside `/Applications`.
- Each session preserves compact evidence, then cleans its owned disposable probes and completed
  clean/pushed worktrees. Coordinate active owners first. Preserve active caches, dirty/unpushed work
  and all local/remote branches. Never archive `target` or `node_modules`; pass this lifecycle to delegates.

## Git

- Branch from fresh `origin/main` in your own worktree. Check `git branch --show-current` first.
- Stage explicit paths. Never `git add -A`. A stray change you did not make is another session's.
- Commit through the hook (`make hooks` enables it). Never `--no-verify`.
- End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- `Closes #N` only for the complete result; `Refs #N` otherwise.
- Squash-merge after the three required checks pass:
  `rust (macos-latest)`, `rust (windows-latest)`, `ui`.

CI quirks:

- `rerun-no-runner.yml` automatically re-runs `rust (macos-latest)` once on the current head after a
  zero-step runner acquisition cancellation. If the job is still cancelled with zero steps after that
  workflow ran (no attempt 2, or attempt 2 cancelled the same way) and the run's commit is still the
  PR head, run `gh run rerun <run-id> --failed`. A stale commit needs no rerun: the new head has its own run.
- Branch protection is strict. After another merge, run `gh pr update-branch <n>`.
- Auto-merge sometimes stalls at CLEAN. Then merge with `gh pr merge <n> --squash`.

## Verify

- Run `make check-changed` once before review; `ARGS=--plan` shows what it will run and why.
  `make check` runs everything CI runs.
- A failed, skipped or interrupted check is not a pass.
- Golden refs change only on purpose: `make golden-update`, and say why in the PR.
- Write tests for real-time safety, determinism and golden renders, data writes and file formats,
  crash safety, security boundaries, and bugs that already happened.
- Never test copy, markup or wiring.
- UI logic lives in plain `.ts` modules. Test it there.
- UI behaviour is proved by a live fixture and a Playwright pass, not a new mocked component test.

## Review

- Every behaviour change gets an independent review on a different model family than the implementer.
- Reviewer: GPT 6.1 Sol (`gpt-6.1-sol` on Codex, high effort), each round a fresh `delegate_task`
  with `role: "review"`. Fallback: GPT 6 Astra, then the thread's own model; say so in the PR.
- The reviewer is read-only. It replies:

  ```text
  VERDICT: APPROVE | CHANGES
  R1 [blocker|should|nit] <file>:<line> <claim>. Fix: <suggestion>
  ```

- Check every finding yourself. Fix, or disagree with evidence. Keep a ledger:
  id, severity, fixed / disagreed / declined, evidence.
- Budget: 3 rounds; 5 for data loss, real-time, determinism or security.
  An item disputed twice, or an exhausted budget, goes to the owner.
- Real-time path, file format, crash safety, determinism and security changes also get a QA pass.
  Post its report as a PR comment in four sections: Worked / Failed / Blocked / Follow-up.

## Documentation

- Most changes need no documentation. Agents can read the code.
- Write a trap note only for what an agent would get wrong from the code alone.
- A local reason goes in a short comment next to the code. A mechanical rule becomes a test.
- Never add feature descriptions, file catalogs, QA diaries or task summaries.
  GitHub issues own plans, history, progress and evidence.
- README.md never carries milestones, a roadmap or feature status.
- Specs, plans and evidence stay untracked under `docs/superpowers/`, `docs/evidence/`, `.superpowers/`.
  Pre-repo mockups and prototypes live untracked in `docs/superpowers/archive/`. Never commit them.
- When a rule changes, rewrite or delete the old text. Do not append.

## Communication

- Answer first. Plain words, short sentences.
- Bullets for three or more facts. No filler or closing summaries.
