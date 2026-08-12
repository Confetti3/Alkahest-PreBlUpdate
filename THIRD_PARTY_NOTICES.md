# Third-party notices

Alkahest Pre-BL is distributed under GPL-3.0-only. The release archive also contains or embeds the following third-party components. The generated SPDX SBOM attached to each release is the authoritative version inventory.

## SDL 3

- Source: https://github.com/libsdl-org/SDL
- Component: `SDL3.dll`
- License: zlib License
- Modifications: none; redistributed as the runtime library used by the Rust `sdl3` binding.

## Google Material Symbols

- Source: https://github.com/google/material-design-icons
- Component: Material Symbols font bytes embedded through `google-material-symbols`
- License: Apache License 2.0
- Modifications: glyphs are rendered by the application; the font itself is not modified.

## Rust dependencies and embedded default fonts

Cargo registry and git dependencies, including egui's default fonts, are enumerated with source, version, checksum, and declared license in `alkahest-prebl.spdx.json` attached to every release candidate.

## User-supplied game data

No Bungie package data is included in release archives. Users must supply their own legally obtained Destiny 2 package corpus. This project is an independent inspection tool and is not affiliated with, endorsed by, or sponsored by Bungie, Inc.
