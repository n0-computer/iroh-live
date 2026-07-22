# SUPERSEDED - do not use

This map described moq main when it was thin (moq-video 0.0.4, H.264
ffmpeg encode only, no decode). On 2026-07-21 moq merged dev into main,
so main now carries the full native stack. The current maps are:

- `maps/moq-video.md` (moq main HEAD 3a3e0ea8)
- `maps/moq-audio-nvenc.md`
- `maps/moq-transcode-stats.md`
- `maps/moq-net-origin.md`

The former "dev" framing throughout the plan set is obsolete: everything
those maps described as dev-only is now on moq main.
