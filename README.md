# Alkahest Pre-BL

Alkahest Pre-BL is a Windows map viewer and resource inspector for user-supplied
Destiny 2 Shadowkeep / Season of Arrivals package files. Current live-game
packages are not supported.

This repository is a fork of [Alkahest](https://github.com/cohaereo/alkahest),
created by [cohaereo](https://github.com/cohaereo/cohaereo) and
[Alkahest's contributors](https://github.com/cohaereo/alkahest/graphs/contributors).
This project builds on their original architecture, renderer, and tooling.

## Install a release

Requirements:

- 64-bit Windows 10 or Windows 11
- A Direct3D 11-capable graphics adapter
- A legally obtained, preserved Shadowkeep / Season of Arrivals client

1. Download the latest Windows archive from
   [GitHub Releases](https://github.com/Confetti3/Alkahest-PreBlUpdate/releases/latest).
2. Extract the complete archive to a writable directory. Keep
   `alkahest-prebl.exe`, `SDL3.dll`, and `wordlist.txt` together.
3. Run `alkahest-prebl.exe`.
4. Select one of the following when prompted:
   - the preserved client directory containing `packages`;
   - the `packages` directory itself; or
   - a DepotDownloader root containing
     `depots\1085661\<install>\packages`, such as `G:\depot`.

The selected package location is remembered. Game data is read in place and is
not copied into the application directory.

## Build from source

Install Git and Rust, then use the toolchain pinned by `rust-toolchain.toml`:

```powershell
git clone https://github.com/Confetti3/Alkahest-PreBlUpdate.git
cd Alkahest-PreBlUpdate
rustup show
cargo build --profile dist --locked --no-default-features --bin alkahest-prebl
.\target\dist\alkahest-prebl.exe --version
```

Run `target\dist\alkahest-prebl.exe`. The build script places the required
`SDL3.dll` in the same directory. The external `wordlist.txt` file must also be
beside the executable when it is moved out of the repository.

The map audio-placement filter is part of the default build. Actual Wwise audio
playback is optional and requires a locally installed Wwise SDK plus
`--features wwise`.

To verify the complete source tree before packaging:

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked --no-default-features
cargo test --workspace --all-targets --locked --no-default-features
```

## Troubleshooting

- Ensure you are running `target\dist\alkahest-prebl.exe`, not an older binary
  from `target\debug` or `target\release`.
- Confirm that the selected packages belong to the preserved Shadowkeep /
  Season of Arrivals client.
- Keep `SDL3.dll` and `wordlist.txt` beside the executable.
- Update your graphics driver if Direct3D 11 initialization fails.
- Logs are written to
  `%LOCALAPPDATA%\cohae\alkahest-prebl\data\alkahest-prebl.log`.

## Data, licensing, and affiliation

Alkahest Pre-BL distributes no Bungie game data. Users must supply their own
legally obtained package corpus. This project is independent and is not
affiliated with, endorsed by, or sponsored by Bungie, Inc.

The source is licensed under [GPL-3.0-only](LICENSE). See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for redistributed components.

## Verify a release

Release downloads include checksums and GitHub artifact attestations:

```powershell
Get-FileHash .\alkahest-prebl-v0.7.0-rc.2-windows-x64.zip -Algorithm SHA256
gh attestation verify .\alkahest-prebl-v0.7.0-rc.2-windows-x64.zip --repo Confetti3/Alkahest-PreBlUpdate
```
