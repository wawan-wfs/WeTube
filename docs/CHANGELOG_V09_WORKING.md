# v0.9.0 working checkpoint (not a release)

- Materialized all 120 upstream blobs with matching original Git tree in the repository.
- Reconstructed native source candidate using v0.8.0 source patches and exact migration/session modules; regenerated seven branding variants.
- Committed Windows manual-only workflow with separate source verification and artifact job, locked Cargo check/test/build, executable artifact if successful.
- Local baseline with V09 integrity patch: 56 utility tests passed. Remote Rust, GUI and player tests not yet run. The reconstructed tree is not claimed byte-identical to all generated v0.8.0 files.
