use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncWriteExt};
use tokio::sync::RwLock;
use tokio_util::io::ReaderStream;
use tower_http::{
    cors::CorsLayer,
    limit::RequestBodyLimitLayer,
    trace::TraceLayer,
};
use tracing::info;
use utoipa::{OpenApi, ToSchema};

use utoipa_swagger_ui::SwaggerUi;
use uuid::Uuid;

#[derive(Clone, Default)]
struct AppState {
    inner: Arc<RwLock<Store>>,
}

#[derive(Default)]
struct Store {
    albums: HashMap<String, Album>,
    tracks: HashMap<String, Track>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
struct Album {
    id: String,
    title: String,
    artist: String,
    price_cents: i64,
    currency: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
struct CreateAlbumRequest {
    title: String,
    artist: String,
    price_cents: i64,
    currency: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
struct Track {
    id: String,
    album_id: String,
    title: String,
    /// Order inside the album. Lower comes first.
    order: i32,
    duration_ms: i64,
    /// Public dev URL that streams the uploaded audio.
    audio_url: String,
    file: Option<FileMeta>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
struct FileMeta {
    filename: String,
    content_type: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
struct CreateTrackRequest {
    title: String,
    order: Option<i32>,
    duration_ms: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
struct ErrorBody {
    code: ApiErrorCode,
    message: String,
    details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum ApiErrorCode {
    bad_request,
    not_found,
    conflict,
    payload_too_large,
    virus_scan_unavailable,
    virus_detected,
    internal,
}

#[derive(Debug)]
struct ApiError {
    code: ApiErrorCode,
    message: String,
    details: Option<String>,
    status: StatusCode,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: ApiErrorCode::bad_request,
            message: message.into(),
            details: None,
            status: StatusCode::BAD_REQUEST,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: ApiErrorCode::not_found,
            message: message.into(),
            details: None,
            status: StatusCode::NOT_FOUND,
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            code: ApiErrorCode::conflict,
            message: message.into(),
            details: None,
            status: StatusCode::CONFLICT,
        }
    }

    fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            code: ApiErrorCode::payload_too_large,
            message: message.into(),
            details: None,
            status: StatusCode::PAYLOAD_TOO_LARGE,
        }
    }

    fn virus_scan_unavailable(message: impl Into<String>) -> Self {
        Self {
            code: ApiErrorCode::virus_scan_unavailable,
            message: message.into(),
            details: None,
            status: StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn virus_detected(message: impl Into<String>, details: impl Into<String>) -> Self {
        Self {
            code: ApiErrorCode::virus_detected,
            message: message.into(),
            details: Some(details.into()),
            status: StatusCode::UNPROCESSABLE_ENTITY,
        }
    }

    fn internal(message: impl Into<String>, details: impl Into<String>) -> Self {
        Self {
            code: ApiErrorCode::internal,
            message: message.into(),
            details: Some(details.into()),
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            code: self.code,
            message: self.message,
            details: self.details,
        };
        (self.status, Json(body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn uploads_root() -> PathBuf {
    // Keep uploads outside git in dev by default.
    std::env::var("MUSIC_UPLOADS_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("uploads"))
}

fn parse_env_u64(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

#[utoipa::path(get, path = "/health", responses((status = 200, description = "OK")))]
async fn health_endpoint() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

#[utoipa::path(
    post,
    path = "/albums",
    request_body = CreateAlbumRequest,
    responses((status = 201, description = "Created", body = Album))
)]
async fn create_album(
    State(state): State<AppState>,
    Json(req): Json<CreateAlbumRequest>,
) -> ApiResult<impl IntoResponse> {
    if req.title.trim().is_empty() || req.artist.trim().is_empty() {
        return Err(ApiError::bad_request("title and artist are required"));
    }
    if req.price_cents < 0 {
        return Err(ApiError::bad_request("price_cents must be >= 0"));
    }

    let album = Album {
        id: Uuid::new_v4().to_string(),
        title: req.title,
        artist: req.artist,
        price_cents: req.price_cents,
        currency: req.currency,
    };

    let mut store = state.inner.write().await;
    store.albums.insert(album.id.clone(), album.clone());
    Ok((StatusCode::CREATED, Json(album)))
}

#[utoipa::path(get, path = "/albums", responses((status = 200, body = Vec<Album>)))]
async fn list_albums(State(state): State<AppState>) -> impl IntoResponse {
    let store = state.inner.read().await;
    let mut albums: Vec<Album> = store.albums.values().cloned().collect();
    albums.sort_by(|a, b| a.title.cmp(&b.title));
    Json(albums)
}

#[utoipa::path(
    get,
    path = "/albums/{album_id}",
    params(("album_id" = String, Path, description = "Album id")),
    responses((status = 200, body = Album))
)]
async fn get_album(
    State(state): State<AppState>,
    Path(album_id): Path<String>,
) -> ApiResult<Json<Album>> {
    let store = state.inner.read().await;
    let album = store
        .albums
        .get(&album_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("album not found"))?;
    Ok(Json(album))
}

#[utoipa::path(
    post,
    path = "/albums/{album_id}/tracks",
    params(("album_id" = String, Path, description = "Album id")),
    request_body = CreateTrackRequest,
    responses((status = 201, description = "Created", body = Track))
)]
async fn create_track_metadata(
    State(state): State<AppState>,
    Path(album_id): Path<String>,
    Json(req): Json<CreateTrackRequest>,
) -> ApiResult<impl IntoResponse> {
    if req.title.trim().is_empty() {
        return Err(ApiError::bad_request("title is required"));
    }
    if req.duration_ms < 0 {
        return Err(ApiError::bad_request("duration_ms must be >= 0"));
    }
    if let Some(o) = req.order {
        if o < 0 {
            return Err(ApiError::bad_request("order must be >= 0"));
        }
    }

    let mut store = state.inner.write().await;
    if !store.albums.contains_key(&album_id) {
        return Err(ApiError::not_found("album not found"));
    }

    let mut existing: Vec<&Track> = store.tracks.values().filter(|t| t.album_id == album_id).collect();
    existing.sort_by(|a, b| a.order.cmp(&b.order));

    let next_order = req
        .order
        .or_else(|| existing.last().map(|t| t.order + 1))
        .unwrap_or(0);

    if existing.iter().any(|t| t.order == next_order) {
        return Err(ApiError::conflict(format!(
            "order {} already exists in album",
            next_order
        )));
    }

    let track_id = Uuid::new_v4().to_string();
    let audio_url = format!("/tracks/{track_id}/file");

    let track = Track {
        id: track_id.clone(),
        album_id: album_id.clone(),
        title: req.title,
        order: next_order,
        duration_ms: req.duration_ms,
        audio_url,
        file: None,
    };

    store.tracks.insert(track_id, track.clone());
    Ok((StatusCode::CREATED, Json(track)))
}

#[utoipa::path(
    get,
    path = "/albums/{album_id}/tracks",
    params(("album_id" = String, Path, description = "Album id")),
    responses((status = 200, body = Vec<Track>))
)]
async fn list_tracks_for_album(
    State(state): State<AppState>,
    Path(album_id): Path<String>,
) -> ApiResult<Json<Vec<Track>>> {
    let store = state.inner.read().await;
    if !store.albums.contains_key(&album_id) {
        return Err(ApiError::not_found("album not found"));
    }

    let mut tracks: Vec<Track> = store
        .tracks
        .values()
        .filter(|t| t.album_id == album_id)
        .cloned()
        .collect();
    tracks.sort_by(|a, b| a.order.cmp(&b.order));
    Ok(Json(tracks))
}

#[utoipa::path(
    post,
    path = "/tracks/{track_id}/file",
    params(("track_id" = String, Path, description = "Track id")),
    responses((status = 200, body = Track))
)]
async fn upload_track_file(
    State(state): State<AppState>,
    Path(track_id): Path<String>,
    mut multipart: Multipart,
) -> ApiResult<impl IntoResponse> {
    let max_audio_bytes = parse_env_u64("MAX_AUDIO_BYTES", 25 * 1024 * 1024);
    let uploads_dir = uploads_root();
    let track_dir = uploads_dir.join("tracks").join(&track_id);

    {
        let store = state.inner.read().await;
        if !store.tracks.contains_key(&track_id) {
            return Err(ApiError::not_found("track not found"));
        }
    }

    fs::create_dir_all(&track_dir)
        .await
        .map_err(|e| ApiError::internal("failed to create upload directory", e.to_string()))?;

    let tmp_path = track_dir.join(format!("tmp-upload-{}.bin", Uuid::new_v4()));
    let mut tmp_file = fs::File::create(&tmp_path)
        .await
        .map_err(|e| ApiError::internal("failed to create temp upload file", e.to_string()))?;

    let mut found = false;
    let mut original_filename = "audio".to_string();
    let mut content_type = "application/octet-stream".to_string();
    let mut written: u64 = 0;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
    {
        if field.name() != Some("file") {
            continue;
        }
        found = true;

        if let Some(fn_) = field.file_name() {
            if !fn_.trim().is_empty() {
                original_filename = fn_.to_string();
            }
        }
        if let Some(ct) = field.content_type() {
            if !ct.trim().is_empty() {
                content_type = ct.to_string();
            }
        }

        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| ApiError::bad_request(e.body_text()))?
        {
            written += chunk.len() as u64;
            if written > max_audio_bytes {
                let _ = fs::remove_file(&tmp_path).await;
                return Err(ApiError::payload_too_large(format!(
                    "audio file too large (max {} bytes)",
                    max_audio_bytes
                )));
            }
            tmp_file
                .write_all(&chunk)
                .await
                .map_err(|e| ApiError::internal("failed writing upload", e.to_string()))?;
        }
    }

    if !found {
        let _ = fs::remove_file(&tmp_path).await;
        return Err(ApiError::bad_request("missing multipart field 'file'"));
    }

    tmp_file
        .flush()
        .await
        .map_err(|e| ApiError::internal("failed flushing upload", e.to_string()))?;

    scan_for_viruses_with_clamav(&tmp_path).await?;

    let ext = std::path::Path::new(&original_filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin");
    let final_path = track_dir.join(format!("audio.{ext}"));
    fs::rename(&tmp_path, &final_path)
        .await
        .map_err(|e| ApiError::internal("failed finalizing uploaded file", e.to_string()))?;

    let mut store = state.inner.write().await;
    let track = store
        .tracks
        .get_mut(&track_id)
        .ok_or_else(|| ApiError::not_found("track not found"))?;
    track.file = Some(FileMeta {
        filename: original_filename,
        content_type,
    });
    Ok((StatusCode::OK, Json(track.clone())))
}

#[utoipa::path(
    get,
    path = "/tracks/{track_id}/file",
    params(("track_id" = String, Path, description = "Track id")),
    responses((status = 200, description = "Audio file bytes"))
)]
async fn stream_track_file(
    State(state): State<AppState>,
    Path(track_id): Path<String>,
) -> ApiResult<Response> {
    let track = {
        let store = state.inner.read().await;
        store
            .tracks
            .get(&track_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("track not found"))?
    };

    let uploads_dir = uploads_root();
    let track_dir = uploads_dir.join("tracks").join(&track_id);
    let mut dir = fs::read_dir(&track_dir)
        .await
        .map_err(|e| ApiError::not_found(format!("uploads missing: {}", e)))?;

    let mut file_path: Option<PathBuf> = None;
    while let Some(entry) = dir
        .next_entry()
        .await
        .map_err(|e| ApiError::internal("failed reading upload dir", e.to_string()))?
    {
        let p = entry.path();
        if p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("audio."))
        {
            file_path = Some(p);
            break;
        }
    }

    let file_path = file_path.ok_or_else(|| {
        ApiError::not_found("audio not uploaded for this track")
    })?;

    let content_type = track
        .file
        .as_ref()
        .map(|f| f.content_type.clone())
        .unwrap_or_else(|| "application/octet-stream".to_string());

    let file = fs::File::open(&file_path)
        .await
        .map_err(|e| ApiError::internal("failed opening uploaded file", e.to_string()))?;

    let stream = ReaderStream::new(file);
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}

async fn scan_for_viruses_with_clamav(path: &FsPath) -> ApiResult<()> {
    // Safe requirement: if clamscan isn't available, fail uploads.
    let clamscan_found = std::process::Command::new("clamscan")
        .arg("--version")
        .output()
        .is_ok();

    if !clamscan_found {
        return Err(ApiError::virus_scan_unavailable(
            "virus scanning not available (install clamscan to enable uploads)",
        ));
    }

    let out = tokio::process::Command::new("clamscan")
        .arg("--no-summary")
        .arg("--infected")
        .arg(path)
        .output()
        .await
        .map_err(|e| ApiError::internal("failed running clamscan", e.to_string()))?;

    match out.status.code() {
        Some(0) => Ok(()),
        Some(1) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            Err(ApiError::virus_detected("virus detected", stdout))
        }
        _ => Err(ApiError::internal(
            "clamscan failed",
            format!(
                "exit_code={:?} stderr={}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ),
        )),
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        health_endpoint,
        create_album,
        list_albums,
        get_album,
        create_track_metadata,
        list_tracks_for_album,
        upload_track_file,
        stream_track_file
    ),
    components(schemas(ErrorBody, ApiErrorCode, Album, CreateAlbumRequest, Track, FileMeta, CreateTrackRequest)),
    tags((name = "health"), (name = "albums"), (name = "tracks"))
)]
struct ApiDoc;

#[tokio::main]
async fn main() {
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8085);
    let log_level = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());

    tracing_subscriber::fmt()
        .with_env_filter(log_level)
        .init();

    let state = AppState::default();
    let openapi = ApiDoc::openapi();

    let max_audio_bytes = parse_env_u64("MAX_AUDIO_BYTES", 25 * 1024 * 1024);
    let uploads_dir = uploads_root();
    let _ = fs::create_dir_all(uploads_dir.join("tracks")).await;

    let swagger_ui = SwaggerUi::new("/api-docs/swagger-ui/").url(
        "/api-docs/openapi.json",
        openapi.clone(),
    );
    let swagger_router: Router<AppState> = Router::from(swagger_ui);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .expect("invalid HOST/PORT");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind failed");

    info!("music-backend listening on http://{}/", listener.local_addr().unwrap());

    let app = Router::new()
        .route("/health", get(health_endpoint))
        .route("/albums", post(create_album).get(list_albums))
        .route("/albums/:album_id", get(get_album))
        .route(
            "/albums/:album_id/tracks",
            post(create_track_metadata).get(list_tracks_for_album),
        )
        .route("/tracks/:track_id/file", post(upload_track_file).get(stream_track_file))
        .merge(swagger_router)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new((max_audio_bytes + 1024) as usize))
        .with_state(state)
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(ErrorBody { code: ApiErrorCode::not_found, message: "not found".to_string(), details: None })) });

    axum::serve(listener, app).await.unwrap();
}
