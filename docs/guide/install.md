# Install Sieve

Sieve is one program named `sieve`. This page shows four ways to install it.

## Targets

Release 0.1.0 has builds for four targets:

- macOS on arm64 and x64.
- Linux on x64 and arm64.

There is no Windows build.

## Choose a channel

Shell installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/iamalvisng/sieve/releases/latest/download/sieve-cli-installer.sh | sh
```

The installer puts `sieve` in your Cargo home folder (`$CARGO_HOME/bin`).
Make sure that folder is on your `PATH`.

Homebrew:

```sh
brew install iamalvisng/tap/sieve
```

Cargo binstall:

```sh
cargo binstall sieve-cli
```

Build from source with Cargo:

```sh
cargo install --locked --git https://github.com/iamalvisng/sieve sieve-cli
```

## Check the install

```sh
sieve --version
```

Release 0.1.2 prints `0.1.2`.

## Check a downloaded archive

Each release archive has a sha256 file and a GitHub attestation.

1. Download the archive and its sha256 file from the release page.
2. Compare the checksum. On macOS, run `shasum -a 256 <file>`. On Linux, run `sha256sum <file>`.
3. Verify the attestation:

```sh
gh attestation verify <file> --repo iamalvisng/sieve
```

The release also has a software bill of materials (SBOM).

## Updates

The installer does not add an updater program. Sieve runs no update check.
To update, run the install command for your channel again.

## Next step

Go to [Quickstart](quickstart.md).
