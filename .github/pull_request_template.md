<!--
  Fill every section. A pull request that leaves "Where it was tested" or the
  screenshots empty is closed by the triage bot without review — not as
  punishment, but because a change nobody has seen run is not a change anybody
  can merge quickly, and a UI change nobody has seen is not reviewable at all.

  Delete the guidance lines as you go; keep the headings.
-->

## What this changes

<!-- One paragraph. What is different after this change, in behaviour a user could notice. -->

## Why

<!--
  What was wrong, or what was missing. If a fix, name the cause — "the layer
  wrote before the background existed" beats "fixed the ordering". If this
  follows from a design decision, say which one; the design record is not in
  this repository, so the reasoning has to be here or in the commit.
-->

## Where it was tested

A build that compiles is not a fix that works. Say where this was actually run.

- **Tested on:** <!-- emulator, or the exact phone model -->
- **Android version:** <!-- e.g. Android 14 (API 34) -->
- **Vendor shell:** <!-- One UI 6.1, HyperOS 2.0.4.0, stock AOSP … "Android" is not a shell -->
- **Build:** <!-- debug APK from CI, or a local build; say which -->

## Screenshots or a recording

<!--
  Required if anything about the interface changed: layout, spacing, colours,
  icons, a dialog, a panel, the dock, the timeline. Before and after, or a short
  recording. Without this the change cannot be reviewed, and "it looks fine" is
  not something a reviewer can check.
-->

## Verified, and not verified

<!--
  Two lists, and the second one matters more.
  Verified: the checks you actually ran and their result — `cargo test`, the APK
  build, the specific behaviour you exercised by hand.
  Not verified: what you did NOT check, and what a reviewer should therefore
  look at first. "Not verified: anything on a device — no device available" is a
  normal and useful line. Claiming verification that did not happen is not.
-->

## Checklist

- [ ] `cargo test --workspace` passes.
- [ ] The app compiles and the APK builds.
- [ ] No comment in another language: this repository is English throughout.
- [ ] User-facing text is in a string resource, in all locale folders, not inline.
- [ ] Nothing about the build, the signing configuration or the CI was changed without saying so above.
