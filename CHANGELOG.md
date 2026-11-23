# Changelog

All notable changes to this project will be documented in this file.

## [0.3.0] - 2026-11-23

### Features

- *(re)* Drive the capture VM from the host
- *(re)* Add background captures and full report extracts
- *(lib)* Set angle snapping and the debounce time
- *(cli)* Add angle-snapping and debounce commands
- *(cli)* Add udev command
- *(lib)* Add LED time-out, DPI auto flags, custom lift-off and colour intensity
- *(lib)* Add energy saving and lift-off calibration requests
- *(lib)* Energy saving, lift-off calibration and dropped requests
- *(cli)* Add led-timeout, dpi-auto, energy-saving and lift-off calibration
- *(lib)* Decode button assignments (settings blocks 2-4)
- *(lib)* Read, write, back up and restore button assignments
- *(cli)* Add buttons and button commands
- *(lib)* Add and remove profiles, profile colours, factory reset and pairing
- *(cli)* Add profile add/remove/color, reset and pair
- *(re)* Pass replugged USB devices through to the capture VM
- *(re)* Choose the captured device with KONECTL_RE_DEVICE
- *(lib)* Write macros the mouse plays by itself
- *(cli)* Assign macros with konectl button N 'macro:…'
- [**breaking**] Set system controls and report the charging state

### Bug Fixes

- *(re)* Number captures past 99 and read scroll steps as decimal
- *(release)* Keep the release checks working under pipefail
- *(lib)* Renumber profile buttons by slot and check backups fully
- *(cli)* End quietly on a closed stdout and complete original.txt once

### Documentation

- *(re)* Document VM control and background captures
- Document angle snapping and debounce
- Install the udev rule with konectl udev
- Document LED time-out, energy saving and lift-off calibration
- Document button assignments, the block write target and profile colours
- Document profile slots, factory reset and pairing
- Document macros
- Document system controls and the charging state

## [0.2.0] - 2026-11-22

### Bug Fixes

- *(lib)* [**breaking**] Report the mouse firmware instead of the dongle's
- *(cli)* Show the mouse and dongle firmware

### Documentation

- *(readme)* Add install section for crates.io
- Add development and release guide
- List the C compiler in the build requirements
- Correct the firmware versions

## [0.1.0] - 2026-11-22

### Features

- *(lib)* Add Kone Pro Air protocol and device API
- *(cli)* Add konectl command-line tool
- *(udev)* Grant user access to the config interface
- *(re)* Add Windows VM and USB capture tooling
- *(lib)* Read and write saved settings
- *(cli)* Add settings, profile and backup commands

### Documentation

- Add README and protocol notes
- *(re)* Describe the capture workflow and results
- *(protocol)* Document the settings protocol
- *(readme)* Document the settings commands
