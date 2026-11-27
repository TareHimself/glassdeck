# Key scanner firmware

The firmware for the keyboard's RP2040, the chip that reads the 160 Hall-effect sensors and is the USB keyboard. It's a Rust port of Keyglass's C firmware (`keyboard/firmware`) that behaves exactly like it, known C bugs included, until it has been tested side by side with stock on a real board. It reports `glassdeck-scanner 0.0.1`.

| Path | What's in it |
|---|---|
| `src/main.rs` | Boot and the main loop, in the stock firmware's order |
| `src/board.rs` | Pins and the sensor sweep |
| `src/usb.rs` | USB: keyboard, command interface, serial port, with the stock descriptors |
| `src/link.rs` | Serial link to the keyboard's Linux side |
| `src/storage.rs`, `src/lfs_glue.c` | Settings in flash; the factory calibration is read-only |
| `src/recovery.rs` | Three crashes in a row reboot into the USB boot ROM |
| `memory.x` | Keeps the image below the calibration partition at `0xF00000` |
| `littlefs/` | littlefs v2.9.3, the stock version (vendored) |
| `vectors/` | C programs that record what the stock firmware does, for the tests |

The logic lives in `crates/glassdeck-scanner-dsp`, and the wire formats in `crates/glassdeck-scanner-proto`.

## Build

```sh
sh firmware/build.sh
```

This needs Docker, because littlefs is C and needs an Arm C compiler. Output: `firmware/target/thumbv6m-none-eabi/release/glassdeck-scanner.uf2`.

## Flash a keyboard

`glassdeck scanner flash` only runs with a complete backup of the same keyboard. It reads the keyboard's factory calibration before writing anything, and stops if that doesn't match the backup. The calibration can't be remade, so never skip the backup.

### Once per Windows PC: the boot ROM driver

The backup reads flash through the RP2040 boot ROM's USB interface (PICOBOOT, as picotool does). Windows has no driver for it, and the device only appears while a chip is in boot mode.

1. Put the keyboard in boot mode. A drive called RPI-RP2 appears.
   ```powershell
   glassdeck scanner bootloader
   ```
2. Run [Zadig](https://zadig.akeo.ie). Choose **Options > List All Devices**, pick **RP2 Boot (Interface 1)**, choose **WinUSB**, and click **Install**.
3. Unplug and replug the keyboard so it starts its firmware again.

### Every flash

1. Back up to a new folder:
   ```powershell
   glassdeck scanner backup D:\keyboard-backups\stock
   ```
   This saves the settings (keymap, profiles, runtime state, press-refinement model) into `settings/`. It then reboots into the boot ROM and saves all 16 MiB of flash as `flash.bin`, which holds the stock firmware and the settings partition. It also saves the factory calibration beside the dump, checks the whole folder, and reboots the keyboard back into its firmware. `manifest.txt` is written last; a folder without it is unfinished.
2. Flash:
   ```powershell
   glassdeck scanner flash firmware\target\thumbv6m-none-eabi\release\glassdeck-scanner.uf2 --backup D:\keyboard-backups\stock
   ```
3. Check that it's running:
   ```powershell
   glassdeck scanner version
   ```

`glassdeck scanner check-backup <folder>` checks a backup without touching the keyboard.

### Back to stock

```powershell
glassdeck scanner restore --backup D:\keyboard-backups\stock
```

This puts the backup's firmware back. It checks the keyboard's calibration matches the backup first, rewrites only the 4 KiB sectors that differ, reads each one back, then reboots. It works from the running firmware or from the boot ROM, for example after a crash loop. The settings partition is left alone unless you add `--settings`, which also puts back the backup's keymap, profiles and rest references.

### If something goes wrong

| Problem | What to do |
|---|---|
| `backup` fails at "claiming the boot ROM's PICOBOOT interface" | The driver isn't installed. The keyboard is in boot mode right now, so do step 2 of the driver setup, replug, and back up again into a new folder. A backup never reuses a folder. |
| `flash` says another RP2040 is in its boot ROM | Unplug the other board. If it's this keyboard, replug it so it starts its firmware. |
| `flash` says the calibration doesn't match | The backup is of a different keyboard. Back up this one first. |
| The new firmware crashes | After three crashes in a row it reboots into the boot ROM by itself. Run `restore`, or copy a working `.uf2` onto the RPI-RP2 drive. |
| The new firmware misbehaves | `glassdeck scanner restore --backup <folder>` puts stock back. |
