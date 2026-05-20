# music

Rust backend for a minimal Spotify-like MVP (albums -> ordered tracks -> audio upload).

## Dev server

Run:

    cd backend
    HOST=0.0.0.0 PORT=8085 RUST_LOG=info cargo run

Swagger (OpenAPI) for dev:
- GET /api-docs/swagger-ui/
- GET /api-docs/openapi.json

## Core API

    GET  /health
    POST /albums
    GET  /albums
    GET  /albums/{album_id}

    POST /albums/{album_id}/tracks          (create track metadata + order)
    GET  /albums/{album_id}/tracks          (ordered tracks)

    POST /tracks/{track_id}/file           (multipart upload; field name: "file")
    GET  /tracks/{track_id}/file           (stream uploaded audio)

## Upload limits + virus scanning

Uploads are size-limited and scanned before accepting.

Environment variables:
- MAX_AUDIO_BYTES (default: 25 * 1024 * 1024 bytes)
- MUSIC_UPLOADS_DIR (default: uploads/)

Virus scanning:
- Uploads use ClamAV clamscan if it is installed.
- If clamscan is not available, uploads fail with 503 (safe behavior: we do not pretend to scan).

## Keep server alive

Use tmux:

    tmux new -d -s music-backend 'cd projects/music/backend && HOST=0.0.0.0 PORT=8085 RUST_LOG=info cargo run'
    tmux ls

