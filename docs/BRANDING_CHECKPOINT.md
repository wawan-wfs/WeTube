# Branding checkpoint (not a release)

User requirement: keep full product name WeTube Desktop by WFS Project.
- Central product name, credit and full display name used in native UI, window titles, About and audio session name.
- Windows version resource: ProductName and FileDescription use full name, CompanyName WFS Project, upstream MIT copyright retained. Internal package and executable names unchanged.
- Setup display name, installer version info and shortcut comments use full branding. Stable AppId, install directory, Start group, shortcut names and user data paths retained to avoid duplicate entries and preserve compatibility. MIT LICENSE unchanged.
- Installed EXE metadata, setup metadata and native window title checked in the manual acceptance job; no pass claimed until actual Windows run.
- Previous accepted installer run 45658844032 remains unchanged. No run on 34fb463 as of this check; moving video and seek gates pending, YouTube playback and downloads still not established. No release bump (Cargo version remains 0.8.0 while v0.9.0 is in development). New branding requires a new manual CI run. No large feature batch until playback gates are satisfied.
