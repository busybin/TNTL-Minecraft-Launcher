# Mod bisect

The bisect finds the jar that stops the game from reaching the main menu. It is
dependency-aware, and it tests new or never-launched jars first.

## Inputs

- The enabled jar list, with each jar's declared dependencies, `provides` ids
  and nested (jar-in-jar) ids.
- `launch-history.json` in the **instance** directory. A successful launch
  records a fingerprint for every enabled jar:
  `id | version | size | mtime`. Renaming `.jar` to `.jar.disabled` does not
  change the fingerprint; replacing a jar with another build does.
- A jar is **proven** when its exact fingerprint is in that file, and
  **unproven** otherwise (new install, updated build, or first run after the
  feature was added).

## Dependency closure

A test set is not a raw list of files. The planner resolves it:

1. Start with the requested jars.
2. Add every missing dependency provider (prefer jars the user already had on).
3. Repeatedly drop any jar whose dependency has no enabled provider; this
   cascades to everything that depends on it.

Rule 3 is the "stopping one also stops its dependents" requirement. A library
is never switched off while an enabled addon still needs it, and the UI reports
which addons were dragged off by the same step.

Blocked jars (loader/game mismatch) are never enabled by the planner. The
session refuses to start if one is already enabled or if a required dependency
is not installed: a guaranteed failure would make every test meaningless.

## Algorithm

`tntl-bisect` is a deterministic state machine. `tntl-app` applies each test,
launches the game, and feeds back `crashed: bool`.

1. **Reproduce** — all originally enabled jars. If the game reaches the menu,
   stop with "not reproduced".
2. **New-first pool** — unproven jars, newest first.
   - If the pool is empty or covers everything, the suspects are all jars.
   - Otherwise test **pool only** (proven jars off, except dependency providers).
     - Crash: the problem needs only the new jars; suspects are the actually
       enabled set, baseline is empty.
     - No crash: the old jars are part of the trigger; baseline is every proven
       jar and suspects are the pool.
3. **Binary split** — split suspects in half (newest half first); test
   baseline + half. If it crashes, keep that half and repeat. If it does not,
   test the other half. One level costs at most two launches.
4. **Verification** — with one suspect left, test all original jars minus that
   jar. No crash means the jar is the culprit; the report also lists every
   dependent that was switched off by the closure.
5. **Interaction** — if neither half crashes alone, the failure needs a
   combination. The session stops and reports the remaining suspects instead of
   guessing a single jar.

The final one-jar test is important: a jar may belong to a failing set without
being able to crash the game by itself.

Expected launches: `1 (reproduce) + 1 (pool) + 2 * log2(n) + 1 (verify)`, so
about 10 launches for 100 mods. The UI shows the estimate before starting.

## Launch probe

- The game gets its own log file (`logs/bisect-<n>-<stamp>.log`) with
  `detach_output`, so no pipe stalls when the launcher waits.
- A launch counts as **passed** when the log contains `Sound engine started`
  (or `OpenAL initialized` together with `Backend library:`). The game is then
  killed after a two-second settle delay. A crash after the menu is not a
  startup failure.
- A process exit without the marker is a **crash** (non-zero exit, or a
  classified crash report). Exit code 0 without the marker and a timeout
  without the marker are **ambiguous**: the session stops and points at the log
  instead of guessing, because one wrong observation would poison the search.
- Each launch is claimed with the normal `.running` marker, so a second Play
  click cannot start a parallel game.

## Safety

- The original enabled/disabled state is captured before the first test and
  restored on success, error, cancel and launcher-side panic (Drop).
- Bisect launches are **not** written to `launch-history.json`: testing two jars
  is not proof that they work together.
- `bisect-last.json` records the outcome, culprit file and launch count for
  support.
- No video card / display assumption: the probe only reads the game log.

## Limitations

- It finds one culprit. Two independent crashing jars are reported as an
  interaction after the first branch is followed.
- A dependency closure can force a jar back on or off; the report shows the
  actual enabled set for every test.
- A flaky crash (random) cannot be distinguished from a deterministic one.
  Raise the per-launch timeout before starting.
- The planner never edits config files, only renames jars.
