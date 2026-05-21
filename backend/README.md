# music-backend

Minimal backend for the "music" MVP (no auth yet).

## Run locally

From this folder:

cargo run

It listens on:

- http://127.0.0.1:8085/health
- Swagger UI (dev docs): http://127.0.0.1:8085/api/docs

## Endpoints

GET /health
POST /albums
GET /albums
GET /albums/{id}
POST /albums/{id}/tracks
GET /tracks/{id}

## Sample payloads

POST /albums
{
"title": "Album Title",
"artist": "Artist Name",
"price_cents": 499,
"currency": "USD"
}

POST /albums/{id}/tracks
{
"title": "Track Title",
"duration_ms": 210000,
"audio_url": "https://example.com/track.mp3"
}
