# winnotch

A macOS-style notch / Dynamic Island for Windows. It sits at the top-center of your screen as a small black pill and expands into a handy panel: now playing with synced lyrics, a file tray, a focus timer, water reminders, a to-do list and system pop-ups.

## Getting started

1. Download `winnotch.exe`.
2. **Double-click it.** That's it. The notch appears at the top-center of your screen. There is no installer and no window to configure.

Windows may show a "Windows protected your PC" (SmartScreen) message the first time. Click **More info → Run anyway**.

### Starts with Windows

The first time you run it, winnotch copies itself to `%LOCALAPPDATA%\Programs\winnotch\` and adds that copy to your startup apps, so it comes back every time you sign in, without a security prompt. You can run the downloaded exe from anywhere (even Downloads) and delete it afterwards.

To update, download the new `winnotch.exe` and double-click it once; it replaces the installed copy.

## How to quit

winnotch has no window or close button, so to close it:

1. Press **Ctrl + Shift + Esc** to open **Task Manager**.
2. Find **winnotch** (under *Apps* or *Background processes*).
3. Right-click it and choose **End task**.

### Stop it from starting with Windows

Either:

- Open **Task Manager → Startup apps**, right-click **Winnotch** and choose **Disable**, or
- Press **Win + R**, run `"%LOCALAPPDATA%\Programs\winnotch\winnotch.exe" --no-autostart`, and then end the task as above. This removes the startup entry.

### Uninstall

End the task, remove it from startup as above, then delete the folder `%LOCALAPPDATA%\Programs\winnotch` (and `%APPDATA%\winnotch` if you also want to erase your saved to-dos and water progress).

## What it does

Hover the notch to expand it, move away to collapse it. The panel has five tabs.

| Tab | What it does |
| --- | --- |
| **Nook** | Now playing with album art, play / pause / skip, and **synced lyrics**. Click a lyric line to jump to that part of the song. Works with Spotify, browsers (YouTube etc.), Windows Media Player and other players that show up in Windows' media controls, plus VLC. |
| **Tray** | A shelf for files. **Drag a file onto the notch** to drop it in, or press *+ Add files*. **Drag a tile back out** into a folder, the desktop or any app to paste a copy. Click a tile to open it; right-click it or press **×** to remove it. |
| **Focus** | A focus / break timer (Pomodoro style) with adjustable length. |
| **Water** | Tracks how much water you drink today and reminds you during the day until you reach your goal. |
| **To-do** | A task list with reminders. End a task with a time, like `call mom 14:30` or `pay rent at 9:05pm`, and the notch alerts you when it's due. |

Also:

- **System pop-ups** for volume, brightness, Caps Lock, keyboard language, charger / battery, Wi-Fi and headphones / audio-output changes.
- **Idle cat**: when nothing is playing, the notch shrinks into a little cat face.

### Using the file tray

- Drag a file toward the top-center of the screen. The notch opens by itself, then drop the file on the Tray.
- The tray keeps its own copy, so the tile still works even if you move or rename the original.
- Dragging a tile out copies the file; the tile stays in the tray.
- Tray copies are temporary and are cleared the next time winnotch starts.

## Privacy

Everything stays on your PC. The only network request is for lyrics: while the lyrics panel is open, winnotch looks up the current song's title and artist on [LRCLIB](https://lrclib.net).

Your to-dos, water progress and lyrics timing are saved in `%APPDATA%\winnotch`.

## Troubleshooting

**Lyrics are early or late.** Use the lyrics timing adjustment in the Nook; it is remembered between runs.

**Dragging a tile out doesn't work.** Make sure winnotch and the folder or app you drop onto are running at the same privilege level (neither "as administrator", or both). If it still fails, include `%LOCALAPPDATA%\winnotch\drag.log` when you report the problem.

**Nothing shows up.** Check Task Manager to see if winnotch is already running, end it, and start it again.

## For developers

Written in Rust with [egui](https://github.com/emilk/egui) / eframe. To build from WSL / Linux:

```sh
sudo apt install mingw-w64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

The exe is at `target/x86_64-pc-windows-gnu/release/winnotch.exe`. On Windows, `cargo build --release` works directly.

## License

[MIT](LICENSE)
