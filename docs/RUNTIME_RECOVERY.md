# Runtime and installer recovery — v0.9.0 candidate

User evidence: Windows UI opened, but Watch displayed "Could not start the video engine (libmpv): libmpv.2.dylib: LoadLibraryExW failed"; Downloads displayed "Failed: yt-dlp failed". These are actual user screenshots, not CI screenshots. The prior artifact contained only wetube.exe. The first error is consistent with absent/incompatible Windows runtime; exact second error cause is unknown.

- Success-only loader cache; Windows missing-DLL message and no release build-machine DLL fallback; safe absolute paths. Retry no longer permanently caches a failed load.
- Native executable --runtime-self-test uses the application's software libmpv renderer with local image fixture, checks decoded non-black frame and emits JSON. Not a YouTube playback test.
- yt-dlp error classification preserves exit code, omits raw stderr credentials/URLs, suggests ordinary settings/authorized access. No promise of provider access or download success.
- Online Inno Setup candidate fetches the exact original distributor archive using verified SHA-256, extracts only libmpv-2.dll via system tar, verifies DLL hash and installs beside EXE. No bundled third-party DLL redistribution. Network and working Windows runtime dependencies required. Original license attribution retained, runtime notice separate. No Authenticode signing claimed.
- New manual workflow installer job uses same-commit executable; verifies installed hashes, renderer, native window creation and uninstall before uploading setup. Actual GUI interaction and screenshot not established.
- No automatic trigger; no version bump.

Previous gate run 45630596693 passed baseline 33 tests (18 ignored), release and local WAV probe. Run 45648710302 at dabea585 was still in progress when checked; source and cargo check passed, cargo test underway. New patch has not run in CI. Installer candidate not available until new manual run passes. Preserve previous evidence. Full parity not claimed.
