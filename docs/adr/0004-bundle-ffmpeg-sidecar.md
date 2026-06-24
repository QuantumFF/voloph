# Bundle ffmpeg as a Tauri sidecar

Media work that doesn't go through the webview — audio extraction for segmentation now, and cutting for export later — is handled by **ffmpeg bundled as a Tauri sidecar binary**, rather than a pure-Rust decoder.

Chosen because ffmpeg covers both jobs with one tool and handles whatever codecs the user's camera produces, avoiding the per-codec gaps a pure-Rust decoder (e.g. symphonia) would hit. Playback itself stays in the webview `<video>` element and does not use ffmpeg.

Constraints to respect, not visible in code: this enlarges the installer, and ffmpeg's licensing (LGPL vs GPL builds) must be tracked when distributing — pick the build and document the linkage before shipping.
