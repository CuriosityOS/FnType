<p align="center">
  <img src="assets/FnType-icon.png" width="152" alt="FnType icon">
</p>

<h1 align="center">FnType</h1>

<p align="center"><strong>Hold Fn. Speak. Release. Paste.</strong></p>

<p align="center">
  A native macOS push-to-talk dictation app built in Rust and GPUI, powered by xAI realtime speech-to-text.
</p>

<p align="center">
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-black.svg"></a>
  <img alt="macOS 14+" src="https://img.shields.io/badge/macOS-14%2B-black.svg">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-GPUI-black.svg">
</p>

## Why FnType

FnType stays out of the way: no Dock icon, no editor window, and no subscription. Hold Globe/Fn in any app, dictate, then release. A compact monochrome waveform confirms capture while audio streams directly to xAI over a warm WebSocket.

- **Push-to-talk anywhere** — hold Globe/Fn, release to finish.
- **Fast streaming STT** — PCM16 at 16 kHz in 100 ms frames.
- **Native UI** — a tiny GPUI waveform overlay and menu-bar controls.
- **Personal dictionary** — recognition bias, exact casing, and spoken replacements.
- **Paste-only by default** — automatic Return is opt-in.
- **Safety first** — blocks password fields, avoids Return in terminals by default, and restores the clipboard.
- **Local secret storage** — xAI keys go to macOS Keychain when available.
- **Launch at login** — the installer adds FnType to Login Items.

## Requirements

- Apple Silicon Mac
- macOS 14 or later
- Rust toolchain and Xcode Command Line Tools
- An [xAI API key](https://console.x.ai/) with Voice access and available credit

FnType sends captured audio to xAI only while Fn is held. xAI API usage is billed by xAI; FnType itself has no subscription.

## Install from source

```bash
git clone https://github.com/CuriosityOS/FnType.git
cd FnType
./scripts/install.sh
```

This builds a release binary, installs `/Applications/FnType.app`, adds it to Login Items, and launches it. To skip launch-at-login registration:

```bash
./scripts/install.sh --no-login-item
```

Approve these macOS permissions when prompted:

1. **Microphone** — capture speech.
2. **Input Monitoring** — detect Globe/Fn globally.
3. **Accessibility** — paste into the target app and optionally press Return.

Relaunch FnType after granting Input Monitoring or Accessibility.

### Configure the xAI key

1. Create a restricted key in the xAI Console with Voice access.
2. Copy the key.
3. Click the menu-bar item named **fn**.
4. Choose **Import API key from clipboard**.

FnType saves the key in Keychain and clears the clipboard. If Keychain is unavailable, it uses a mode-`0600` fallback at:

```text
~/Library/Application Support/FnType/.xai-key
```

Never commit or paste an API key into an issue.

## Use

1. Focus a text field.
2. Hold Globe/Fn.
3. Speak.
4. Release Globe/Fn.
5. FnType pastes the final transcript.

Using Fn with another key cancels the recording so normal Fn shortcuts continue to work.

The **fn** menu provides:

- Connection status and reconnect
- API-key import
- Dictionary editing
- Optional Return after insert
- Optional Return in terminals
- Permission prompts
- Quit

## Dictionary

Choose **fn → Edit dictionary…**, or edit:

```text
~/Library/Application Support/FnType/dictionary.txt
```

One entry per line:

```text
# Bias recognition and enforce exact casing
xAI
FnType

# Replace a spoken form in the final transcript
gee pee you -> GPU
open whisper -> OpenWispr
```

Comments and blank lines are ignored. FnType sends up to 100 unique terms to xAI and also applies case-insensitive word-boundary replacements locally.

## Settings

Settings live at:

```text
~/Library/Application Support/FnType/config.json
```

Defaults:

```json
{
  "language": "en",
  "auto_submit": false,
  "allow_terminal_submit": false
}
```

## Safety behavior

FnType does not insert text or press Return when:

- speech is empty,
- transcription fails,
- the focused field is a password field, or
- the original target app disappeared.

Return is skipped in known terminal apps unless explicitly enabled. The previous clipboard text is restored after paste.

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Bundle and install without changing Login Items:

```bash
./scripts/bundle.sh
```

### Stable local signing

Without a signing identity, the bundle script uses an ad-hoc signature. macOS ties privacy grants to that build's code hash, so rebuilding may require permission approval again.

For persistent development permissions, create a self-signed **Code Signing** certificate in Keychain Access named `FnType Local Development`. The bundle script detects and uses it automatically. Override the identity name with:

```bash
FNTYPE_SIGNING_IDENTITY="My Signing Identity" ./scripts/bundle.sh
```

## Architecture

| Module | Responsibility |
| --- | --- |
| `audio.rs` | Microphone capture, device selection, mono downmix, resampling, PCM framing |
| `ws.rs` | Warm xAI WebSocket, buffering, reconnect, STT events |
| `fn_monitor.rs` | Global Fn/Globe monitor and missed-release watchdog |
| `overlay.rs` | Compact monochrome GPUI waveform |
| `inject.rs` | Target activation, secure-field checks, paste and Return |
| `dictionary.rs` | Recognition bias and local replacements |
| `keychain.rs` | Keychain storage with private-file fallback |
| `transcript.rs` | Transcript accumulation and safety policy |
| `main.rs` | App coordination, menu bar, lifecycle, insertion flow |

## Troubleshooting

### Holding Fn does nothing

Remove and re-add `/Applications/FnType.app` under **Privacy & Security → Input Monitoring**, enable it, then relaunch FnType.

### The waveform moves but no text appears

Open the **fn** menu and confirm the status says `xAI realtime ready`. Verify that the API key has Voice access and available credit.

### Dictation is empty, or music sounds tinny, while AirPods are connected

macOS makes AirPods the default microphone. Opening that mic switches them into low-quality call mode and can silence dictation. FnType captures from the Mac's built-in mic instead and temporarily points the system default input there so playback stays in high-quality mode. If the lid is closed or no other mic exists, AirPods are used.

### Permissions reset after rebuilding

Use the stable local signing instructions above. Ad-hoc signatures change identity on each build.

## Privacy and project status

FnType does not persist microphone recordings or transcripts. Audio is transmitted directly to xAI for transcription. Review xAI's policies before use with sensitive material.

This is an independent open-source project and is not affiliated with xAI, Wispr Flow, or OpenWispr.

## License

[MIT](LICENSE) © 2026 CuriosityOS
