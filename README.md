<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/sonago-banner-dark.png">
  <img src="assets/sonago-banner-light.png" alt="Sonago — retrospective audio recording for Windows">
</picture>

Sonago is a native Windows 11 app that lets you save audio after it happens. It runs in the system tray and keeps a rolling history of system audio and microphone input, so you can save the last few minutes whenever you need them.

## Features

- Independent system audio and microphone capture.
- Configurable audio history of up to 12 hours.
- Clip saving with a customizable global shortcut.
- Matroska and Ogg Opus export with separate audio tracks.
- Native WinUI 3 interface with a Rust audio engine.

Audio history is stored locally and cleared when the app exits.

## Download

On Windows 11, open the [latest release](https://github.com/newbialywhodis/hearlier/releases/latest) and download the installer ending in `win-x64-setup.exe` or `win-arm64-setup.exe` for your PC. After installation, open Sonago from the Start menu. Setup includes the Microsoft Visual C++ Redistributable and may ask for administrator approval if it needs to install or update it.

For a portable copy, download the matching ZIP, extract the whole archive, and run `sonago.exe` from the extracted folder. The ZIP bundles .NET and the Windows App SDK and requires the [Microsoft Visual C++ Redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/) for your architecture.

Releases published before the rename use the Hearlier name and include `hearlier.exe`.

## Usage

By default, `Ctrl + Alt + F9` saves the last five minutes of captured audio. You can change the clip duration, capture devices, and keyboard shortcut in settings.

## Building from source

Requirements:

- Windows 11
- .NET 10 SDK
- Rust 1.95+ (MSVC toolchain)
- Visual Studio Build Tools with C++ and Windows SDK
- CMake (available in `PATH` or bundled with Build Tools)

Build and run:

```powershell
.\dev.ps1
```

Build a release:

```powershell
.\dev.ps1 -Configuration Release -NoRun
```

The build is available in `target/winui/win-x64/Release/app` (or `target/winui/win-arm64/Release/app` on ARM64). Run `sonago.exe` from that directory and keep the accompanying files together.

## Publishing a release

Push the commit to GitHub, then tag it with the release version and push the tag.

```powershell
git tag v0.1.0
git push origin v0.1.0
```

GitHub Actions runs the Rust tests and publishes an Inno Setup installer and a portable ZIP for each Windows architecture, with generated release notes. Tags use the `vMAJOR.MINOR.PATCH` format.

## License

Sonago is licensed under the [MIT License](LICENSE). Third-party license notices are included in the `licenses/` directory of each release.
