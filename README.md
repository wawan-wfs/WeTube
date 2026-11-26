# WeTube Desktop by WFS Project

**Native source checkpoint, v0.9.0 release gate open.**

- `upstream-original/`: complete pinned original (120 files; exact tree `9904212b3323512eb009d77322ecf999dcdc92fc`). Its workflows are nested and inactive.
- `native-src/`: actual Rust GPUI/libmpv/SQLite candidate, based on the v0.8.0 source patches. Icon assets recreated from WeTube mark. MIT attribution retained. No wrapper UI.
- `tools/verify_source.py`: verifies actual committed and checkout bytes; exports source ZIP before native compilation.
- `.github/workflows/native-windows.yml`: Windows offline acceptance on push to main or manual dispatch: source, build, libmpv, installer, branding, moving video/seek, native window and uninstall are mandatory. CI runs on main pushes; manual runs remain available in Actions. No automatic release or deployment. Real YouTube playback, clean Windows Vulkan and interactive UI acceptance remain separate gates.

Original upstream commit: f377d0685458ff542f22e7cc98c110a4275b19de. Source checkpoint only; do not publish release until gates pass.
