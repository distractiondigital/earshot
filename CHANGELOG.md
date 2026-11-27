# Changelog

## 1.1.0

### Added

- Switch the microphone or the meeting audio while recording, from the recording card. The track continues in the same file without a break; a device that runs at a different sample rate (such as a Bluetooth headset) is converted to match.
- A warning when the mic or the meeting audio stays silent for 15 seconds after it starts, on the floating recorder (with a Change button that opens the recorder) and on the recording card.
- Audio export options: separate tracks (lossless FLAC, as recorded), a single mixed FLAC, or a single mixed MP3 (smaller, plays anywhere).
- The Earshot logo at the top of the sidebar.

### Fixed

- A microphone opened at a specific sample rate, as when continuing an interrupted recording, could record in an 8-bit format and add faint noise.
- The meeting actions menu no longer wraps its labels onto two lines.

## 1.0.0

First release.
