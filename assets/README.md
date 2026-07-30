# Assets layout

Runtime resources are split by ownership instead of by the platform that happened to be
implemented first.

```text
assets/
├── common/                 # Shared runtime resources
│   ├── licenses/
│   ├── locales/
│   └── tray/
└── platforms/              # Resources shipped only for one target OS
    ├── macos/
    │   ├── sing-box-aarch64
    │   └── sing-box-x86_64
    └── windows/
        ├── libcronet.dll
        └── sing-box.exe
```

Application and installer icons live only in `src-tauri/icons/`, following the Tauri
convention. The images in `common/tray/` are runtime status icons and are intentionally
separate from the application icon.

Platform Tauri configuration maps the selected source directory to `assets/platform/`
inside the bundle. The Universal 2 macOS app selects `sing-box-x86_64` or `sing-box-aarch64`
for its active architecture. Rust otherwise reads the same bundled path on every OS. To add Linux,
create `platforms/linux/` with its sing-box binary, then add a matching
`src-tauri/tauri.linux.conf.json` resource mapping.

The sing-box binaries, Windows runtime DLL, and sing-box license are generated resources and are
ignored by Git. `scripts/fetch-sing-box.mjs` downloads the official release archive for the requested
target, verifies its pinned SHA-256, and extracts these files before development, checks, tests, or
platform builds. Update the version and artifact metadata in that one script when upgrading sing-box.
