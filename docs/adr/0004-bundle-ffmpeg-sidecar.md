# Bundle ffmpeg as a Tauri sidecar

Media work that doesn't go through the webview — audio extraction for segmentation now, and cutting for export later — is handled by **ffmpeg bundled as a Tauri sidecar binary**, rather than a pure-Rust decoder.

Chosen because ffmpeg covers these jobs with one tool and handles whatever codecs the user's camera produces, avoiding the per-codec gaps a pure-Rust decoder (e.g. symphonia) would hit.

Playback stays in the webview `<video>` element. Web-incompatible sources (notably iPhone HEVC, which WebKitGTK cannot decode) are made playable by transcoding them to H.264/AAC with this sidecar; web-friendly H.264/AAC plays untouched. **How** the transcode reaches the webview is decided in ADR 0005 — a once-per-recording transcode performed in place and played via the asset protocol (neither a cached proxy nor a live stream; both earlier attempts were superseded — see ADR 0005). ffmpeg/ffprobe are invoked directly as resolved sidecar paths (`std::process::Command`), not through the shell plugin.

Constraints to respect, not visible in code: this enlarges the installer, and ffmpeg's licensing (LGPL vs GPL builds) must be tracked when distributing — pick the build and document the linkage before shipping.
