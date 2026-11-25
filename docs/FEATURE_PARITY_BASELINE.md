# Feature parity — WeTube Desktop Native v0.8.0

Basis: [Stream Tube Desktop, pinned commit](https://github.com/Avaneesh-Inamdar/Stream-Tube-Desktop/tree/f377d0685458ff542f22e7cc98c110a4275b19de). Matrix is an engineering backlog, **not measured feature equivalence**. Repository tree and selected source files were inspected via GitHub. No complete native repository was materialized or executed in this environment.

Status definitions: **U** = mapped to existing upstream source/module; **P** = mapped to a new/revised WeTube patch; **L** = limited or incomplete; **D** = intentionally disabled pending secure, authorized integration; **N** = not implemented. Every row requires Windows runtime QA before counting as parity. A mapped candidate is not a tested implementation.

| # | Capability | Candidate | Evidence / limitation |
|---:|---|:---:|---|
| 1 | Native GPUI shell | U | `src/main.rs`, `src/ui/app.rs`, GPUI dependencies |
| 2 | Native libmpv playback | U | `src/player/mpv.rs`, no runtime DLL supplied |
| 3 | Home / Explore | U | `src/ui/pages/home.rs` |
| 4 | Search / suggestions | U | `src/ui/pages/search.rs`, `app.rs` |
| 5 | Shorts | U | `src/ui/pages/shorts.rs` |
| 6 | Live playback and chat viewing | U | `src/ui/pages/watch.rs`; network QA needed |
| 7 | Channel pages | U | `src/ui/pages/channel.rs` |
| 8 | Comments and threads | U | `src/ui/pages/watch.rs`, API modules |
| 9 | Remote playlists | U | `src/ui/pages/playlist.rs` |
| 10 | Local playlists and reorder | U | `src/store.rs`, `playlist.rs` |
| 11 | Favorites / Watch Later | P | protected lists plus native migration and transactional deletion patch |
| 12 | Local subscriptions / feed | U | `src/feed.rs`, `subscriptions.rs`, `store.rs` |
| 13 | Local watch and search history | U | `store.rs`, `library.rs`, recording defaults off |
| 14 | Resume playback | U | `src/player/controller.rs`, history progress |
| 15 | Live playback queue | P | imported queue opens as playlist; Play all explicitly loads native queue |
| 16 | Multi-tab browsing | P | tab strip always visible, width adaptation, active border |
| 17 | Back / forward / refresh | P | native forward button and stale-video rebuild |
| 18 | Recently closed video tabs | P | Reopen action and Ctrl+Shift+T from patches |
| 19 | Opt-in video session recovery | P | bounded session module, dirty-gated persistence; not full navigation history |
| 20 | Native PiP | U | `src/ui/pip.rs`; runtime not verified |
| 21 | Miniplayer / theater / fullscreen | U | `src/ui/app.rs`, `src/player/view.rs` |
| 22 | Speed, volume, hardware decoding | U | native player and settings |
| 23 | Captions and chapters | U | player/watch modules; no media QA |
| 24 | Hover previews / seek previews | U | `src/player/preview.rs` |
| 25 | Stats for nerds | U | native player/view candidate |
| 26 | Dark/light/system + 7 accents | P | palette and WeTube icons from v0.7 + native source patch |
| 27 | Content filtering | U | `src/store.rs`, settings |
| 28 | Local recommendations | U | `src/recommend.rs` |
| 29 | Optional third-party metadata tools | U | `src/api/services.rs`; defaults off where patched; availability unverified |
| 30 | Import/export standard upstream formats | U | `src/import.rs`; upstream paths untested |
| 31 | Direct v0.5 library migration | P | new Rust validation and SQLite transaction; Rust integration tests pending |
| 32 | Handle-only channel resolution | L | preserve unresolved handles, copy links; no automatic resolution |
| 33 | Legacy search/session data migration | L | optional search-list extension; v0.5 did not export searches or tabs |
| 34 | Native downloads / external player | U | `src/downloads.rs`, player; only authorized use; redistribution review pending |
| 35 | Google sign-in / account actions | D | external official site only; no cookie/token import |
| 36 | Account history synchronization | D | disabled pending supported authentication design |
| 37 | Automatic signed updates | D | upstream updater disabled; own signed release channel not configured |
| 38 | Single-instance external link handoff | L | command-line link route inherited; insecure upstream loopback disabled |
| 39 | Windows x64 installer and launch | L | Inno Setup config and validation gate; no built installer |

## Counts

- Candidate mapping: **32/39 (82.05%)**, counting U or P only. This is planning/source-path coverage, not runtime success or the completeness of upstream's entire feature list.
- Limited: **4/39 (10.26%)**.
- Intentionally disabled: **3/39 (7.69%)**.
- Native runtime verified: **0/39 (0%)**; no native Windows runtime test was performed. This does not mean the upstream features fail, only that this build has no verified runtime evidence.
- Source manifest metadata: **120/120 tracked files**, reconstructed Git tree matches pinned hash. Full materialized repository: not delivered.

The release gate is a successful locked Windows build, then feature-by-feature actual QA; never interpret the 82.05% source-mapping figure as product completion. For authorized playback, test public or owned media and respect the platform's terms. Keep unsupported account integrations separate from local features.
