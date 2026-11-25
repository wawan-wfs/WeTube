# gpui-pre-windows 0.3.12, patched

Copy of [gpui-pre-windows 0.3.12](https://crates.io/crates/gpui-pre-windows)
(Zed's `gpui_windows` crate, Apache-2.0) with one fix in `src/direct_write.rs`:
right-to-left text (Arabic, Hebrew, ...) was drawn mirrored.

- `DrawGlyphRun`: each run is placed at the origin DirectWrite passes in, and the
  glyphs of a right-to-left run (odd `bidiLevel`) go from that origin leftwards.
  Upstream laid every run out left to right after the previous one, but DirectWrite
  hands runs over in text order and right-to-left runs list their glyphs in reading
  order.
- Tabs are placed where the layout's hit test puts them, for the same reason.
- `StringIndexConverter::advance_to_utf16_ix` can move backwards.
- Tests: `rtl_runs_are_laid_out_right_to_left` is new; the caret checks in
  `tabs_preserve_native_advances_and_cursor` skip right-to-left strings, since they
  assumed the old left-to-right layout.

Every change is marked with "Stream Tube patch". Drop this folder and the
`[patch.crates-io]` entry in `Cargo.toml` once upstream handles bidi text.
