# `fixtures/mobile` — the mobile proof harness

A stock `flutter create` skeleton, committed so the platform has something real
to build when proving mobile execution. It is **not** a product, not a sample,
and nothing imports it.

```
flutter create --platforms=android,ios --org xyz.kasofsk --project-name chug_mobile_proof
```

## What it is for

Two capabilities need an end-to-end proof against real hardware, and both need
an app to point at:

- **Android** — [design #367](../../docs/design/367-android-emulator-execution.md)
  phase A2: `./gradlew` and an emulator, in a container, on a KVM-enabled node.
- **iOS** — [design #322](../../docs/design/322-macos-native-runtime.md): a
  simulator build, host-native on macOS. Blocked much further back than A2 —
  host-native execution does not exist yet
  ([#309](../../docs/design/309-host-native-execution.md) P0/P1) — so the iOS
  half of this fixture is deliberately inert for now.

Flutter, rather than a plain Android project, because that is what the
consuming project uses: a proof that avoids Flutter would not exercise the
toolchain the port actually depends on.

The entrypoint is always a `flutter` command (`flutter build apk`), never a bare
`./gradlew`: the wrapper, `local.properties` — which
`fixtures/mobile/android/settings.gradle.kts` hard-requires — and the generated plugin registrants are gitignored by the stock
template and are written by the Flutter tool on first build. That is also why the
88 generated files land here as 67 tracked ones.

## Why it is generated, and stays generated

Keep it stock. Its value is being *representative* — the moment it acquires
bespoke structure it stops predicting anything about a real app. Regenerate
rather than hand-edit, and if a Flutter upgrade changes the skeleton, take the
new skeleton.

That is also why `fixtures/mobile/**` is in `.jscpd.json`'s `ignorePattern`.
The generated `res/values/styles.xml` and `res/values-night/styles.xml` are a
clone by construction — Android resource qualifiers require two files with the
same shape — so the duplication gate cannot be satisfied by extraction, only by
exclusion. It sits in the config beside `**/*.gen.ts`, and for the same reason:
the Tier 1 duplication rule polices code we author, and this is code we accept.

## What does not apply here

The repo's normal expectations are about code the platform runs. This tree is
input to a job, so `cargo`, the comment lint's two-sentence doc cap and the
MODULES registry have nothing to say about it. `.chug/tasks/ci.sh` gates it
only through the pure-shell stages, which is the whole intent — a change here
should not cost a Rust build.
