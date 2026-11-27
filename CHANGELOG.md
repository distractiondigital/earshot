# Changelog

## 1.2.0

### Added

- Each meeting is saved in its own folder: the audio, a `meeting.json` with the title, tags, transcript (with word timings and corrections), speakers and summary, and a readable `Transcript.md`.
- Meetings in a recordings folder shared through cloud storage (Nextcloud, Google Drive, Dropbox, OneDrive, iCloud Drive) appear on every computer that uses it. Edits, new transcripts and summaries, and deletions carry across; when two computers change the same meeting, the later change is kept. Voice profiles stay on each computer and are matched by name.
- New meeting folders are named by date and time with a short unique id, so meetings from different computers never collide.

### Changed

- Existing meetings get a `meeting.json` in their folder on first launch.
- An interrupted recording can only be recovered on the computer that made it; other computers show it as interrupted there.
- The meeting list shows a speaker's voice-profile name when the speaker has no name of their own.

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
