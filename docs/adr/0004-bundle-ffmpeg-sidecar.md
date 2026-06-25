# Bundle ffmpeg as a Tauri sidecar

Media work that doesn't go through the webview — audio extraction for segmentation now, and cutting for export later — is handled by **ffmpeg bundled as a Tauri sidecar binary**, rather than a pure-Rust decoder.

Chosen because ffmpeg covers these jobs with one tool and handles whatever codecs the user's camera produces, avoiding the per-codec gaps a pure-Rust decoder (e.g. symphonia) would hit.

Playback stays in the webview `<video>` element, but web-incompatible sources (notably iPhone HEVC, which WebKitGTK cannot decode) are **live-transcoded** to H.264/AAC by ffmpeg on play; web-friendly H.264/AAC is served untouched. The transcoded bytes reach the webview over a **loopback HTTP server** (`127.0.0.1`, ephemeral port, per-launch token) that streams ffmpeg's stdout with chunked transfer encoding, so playback starts within ~1s instead of waiting for the whole clip. The server is used because Tauri's custom-protocol responder only accepts a fully-owned body, which would force the entire transcode to be buffered in memory first. Nothing transcoded is written to disk, and seeking a transcoded stream restarts ffmpeg from the target timestamp (`-ss`). ffmpeg/ffprobe are invoked directly as resolved sidecar paths (`std::process::Command`), not through the shell plugin, so stdout can be streamed live.

Constraints to respect, not visible in code: this enlarges the installer, and ffmpeg's licensing (LGPL vs GPL builds) must be tracked when distributing — pick the build and document the linkage before shipping.
