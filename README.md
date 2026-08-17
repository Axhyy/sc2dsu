# SC2DSU

Cemuhook DSU server for the original 2015 and 2026 Steam Controllers, plus the Flydigi Vader 5 Pro. It forwards motion, buttons, sticks, pads, and triggers to emulators such as Cemu, Eden, Citra, and Ryujinx on `127.0.0.1:26760`.

Download `sc2dsu.exe` from [Releases](https://github.com/NightHammer1000/sc2dsu/releases) and run it, then point your emulator at `127.0.0.1:26760`.

Up to four connected controllers are exposed as DSU slots 0–3 in discovery order. Per-slot and per-MAC DSU subscriptions are both supported, so local multiplayer clients only receive the controller slots they request.

If an axis is wrong, swap the source or flip invert in the settings window. Saved live; takes effect on the next IMU sample. Config lives at `%APPDATA%\sc2dsu\config.toml`.

Run modes: `sc2dsu` (GUI + server), `sc2dsu --tray` (start hidden), `sc2dsu --headless` (server only, log to stderr), `sc2dsu --probe` (enumerate Valve and Flydigi HIDs and dump 3 s of decoded IMU).

Tested hardware:

- 2015 Steam Controller over Bluetooth (`28DE:1106`)
- 2026 Steam Controller over the Proteus Puck (`28DE:1304`)
- Flydigi Vader 5 Pro, wired and wireless (`37D7:2401`)

The SDL3-listed 2015 wired (`0x1102`), BLE (`0x1105`/`0x1106`), and wireless dongle (`0x1142`) transports are supported. Triton wired (`0x1302`), BLE (`0x1303`), Proteus (`0x1304`), and Nereid (`0x1305`) use the existing Triton path; transports not listed as tested above still need hardware reports.

## Flydigi Vader 5 Pro

- In the Flydigi Space Station app, enable **"Allow third-party apps to take over mappings"** — without it the pad won't answer the acquire handshake sc2dsu needs to read raw reports.
- Firmware must be at least `7.1.41` (`0x7141`); older firmware doesn't speak the report layout this integration decodes and sc2dsu will refuse to open the device with a firmware-version error rather than emit garbage gyro data. Update via Space Station if you hit this.
- Only the Vader 5 Pro (device ID 130) is recognized today. Other Flydigi V2 pads sharing PID `2401` (Vader 3 Pro, Vader 4 Pro) use the same command channel but a different sensor scale and are rejected until someone adds and verifies their offsets.

Build with `cargo build --release`. CI runs `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo build --release --locked` on every push.

HID protocol from SDL3 [`SDL_hidapi_steam.c`](https://github.com/libsdl-org/SDL/blob/main/src/joystick/hidapi/SDL_hidapi_steam.c), [`SDL_hidapi_steam_triton.c`](https://github.com/libsdl-org/SDL/blob/main/src/joystick/hidapi/SDL_hidapi_steam_triton.c), the [steam protocol headers](https://github.com/libsdl-org/SDL/tree/main/src/joystick/hidapi/steam), and [`SDL_hidapi_flydigi.c`](https://github.com/libsdl-org/SDL/blob/main/src/joystick/hidapi/SDL_hidapi_flydigi.c). DSU protocol from [v1993/gcemuhook](https://github.com/v1993/gcemuhook). MIT.

# Notice on SDL Native Emulators
Emulators that support the Controller Nativly like RPCS3 need the Steam Overlay disabled on their Shortcut to stop Steam Input from Injecting and Hiding the Controller.
SDL Native Programms also do not need this Tool to access Gyro. 
**So. No. You dont need this for RPCS3**


# Like it? Found it useful?

You can help fuel my caffeine addiction here:
https://ko-fi.com/nightstorm1000
