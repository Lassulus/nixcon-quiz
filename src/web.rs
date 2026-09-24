//! HTTP surface: the server-rendered page, one Server-Sent Events stream per
//! player carrying HTML fragments of their view, and one form endpoint to
//! submit an answer.
//!
//! Players are identified by an opaque random token in an HttpOnly cookie,
//! issued on the first page load or event stream without a known one.

use crate::{
    game::{AnswerError, Game, PlayerId, Tally},
    html,
};
use axum::{
    Form, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::stream;
use serde::Deserialize;
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpListener, sync::watch};

const COOKIE: &str = "nixcon_quiz";
/// Long enough to cover a whole conference.
const COOKIE_MAX_AGE: u32 = 14 * 24 * 60 * 60;

struct App {
    game: Mutex<Game>,
    /// Bumped whenever the phase changes; every event stream re-renders.
    changes: watch::Sender<u64>,
    /// Random per process, so versions from before a restart never match.
    boot: u64,
    /// Where players join, for the QR code on the spectator screen.
    public_url: Option<String>,
}

impl App {
    fn game(&self) -> std::sync::MutexGuard<'_, Game> {
        // A panic while holding the lock leaves the game in whatever state it
        // reached; carrying on beats taking the whole quiz down mid-talk.
        self.game.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Names the rendered state: which process, and how many phase changes.
    fn version(&self, changes: u64) -> String {
        format!("{:x}-{changes}", self.boot)
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_millis() as u64
}

/// `public_url` is the address shown to the livestream audience; without it
/// the spectator screen uses the host it was loaded from.
pub async fn serve(
    listener: TcpListener,
    game: Game,
    public_url: Option<String>,
) -> std::io::Result<()> {
    let app = Arc::new(App {
        game: Mutex::new(game),
        changes: watch::Sender::new(0),
        boot: rand::random(),
        public_url,
    });
    tokio::spawn(run_clock(app.clone()));
    let router = Router::new()
        .route("/", get(index))
        .route("/spectate", get(spectate))
        .route("/clock.js", get(clock))
        .route("/style.css", get(style))
        .route("/vendor/{file}", get(vendor))
        .route("/api/events", get(events))
        .route("/api/events/spectate", get(spectate_events))
        .route("/api/answer", post(answer))
        .with_state(app);
    axum::serve(listener, router).await
}

/// Moves the game along at each deadline and wakes every stream.
async fn run_clock(app: Arc<App>) {
    loop {
        let deadline = {
            let mut game = app.game();
            if game.tick(now_ms()) {
                app.changes.send_modify(|n| *n += 1);
            }
            game.deadline()
        };
        let wait = deadline.saturating_sub(now_ms()).max(1);
        tokio::time::sleep(Duration::from_millis(wait)).await;
    }
}

const CSP: &str = "default-src 'self'; frame-ancestors 'none'";

fn static_file(
    content_type: &'static str,
    cache: &'static str,
    body: impl IntoResponse,
) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache),
        ],
        body,
    )
        .into_response()
}

async fn clock() -> Response {
    static_file(
        "text/javascript; charset=utf-8",
        "no-cache",
        include_str!("../static/clock.js"),
    )
}

async fn style() -> Response {
    static_file(
        "text/css; charset=utf-8",
        "no-cache",
        include_str!("../static/style.css"),
    )
}

/// Third-party files, embedded in the binary. Their versions are part of the
/// file names, so browsers may cache them forever.
async fn vendor(Path(file): Path<String>) -> Response {
    const JS: &str = "text/javascript; charset=utf-8";
    const WOFF2: &str = "font/woff2";
    macro_rules! embed {
        ($name:literal) => {
            include_bytes!(concat!("../static/vendor/", $name)).as_slice()
        };
    }
    let (content_type, body): (&str, &'static [u8]) = match file.as_str() {
        "htmx-2.0.11.min.js" => (JS, embed!("htmx-2.0.11.min.js")),
        "htmx-ext-sse-2.2.4.min.js" => (JS, embed!("htmx-ext-sse-2.2.4.min.js")),
        "ibm-plex-mono-latin-400-normal.woff2" => {
            (WOFF2, embed!("ibm-plex-mono-latin-400-normal.woff2"))
        }
        "ibm-plex-mono-latin-600-normal.woff2" => {
            (WOFF2, embed!("ibm-plex-mono-latin-600-normal.woff2"))
        }
        "ibm-plex-mono-latin-ext-400-normal.woff2" => {
            (WOFF2, embed!("ibm-plex-mono-latin-ext-400-normal.woff2"))
        }
        "ibm-plex-mono-latin-ext-600-normal.woff2" => {
            (WOFF2, embed!("ibm-plex-mono-latin-ext-600-normal.woff2"))
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    static_file(content_type, "public, max-age=31536000, immutable", body)
}

fn set_cookie(response: &mut Response, token: &str) {
    let cookie =
        format!("{COOKIE}={token}; Path=/; Max-Age={COOKIE_MAX_AGE}; HttpOnly; SameSite=Strict");
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("hex token"),
    );
}

fn full() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "the quiz is full").into_response()
}

fn html_response(page: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_SECURITY_POLICY, CSP),
        ],
        page,
    )
        .into_response()
}

async fn index(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let now = now_ms();
    let (page, new_token) = {
        let mut game = app.game();
        let Some((id, new_token)) = game.join(token(&headers), now) else {
            return full();
        };
        // The changes counter is only bumped with the game locked, so this
        // version matches the view exactly.
        let events = format!("/api/events?seen={}", app.version(*app.changes.borrow()));
        let page = html::page(&game.view(id, now), &events).into_string();
        (page, new_token)
    };
    let mut response = html_response(page);
    if let Some(token) = new_token {
        set_cookie(&mut response, &token);
    }
    response
}

/// The livestream screen. Watching doesn't make you a player: no cookie, and
/// it doesn't count as online.
async fn spectate(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let join = app.public_url.clone().unwrap_or_else(|| {
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("localhost");
        format!("http://{host}/")
    });
    let game = app.game();
    let page = html::spectate_page(&game.spectate(now_ms()), game.tally(), &join).into_string();
    html_response(page)
}

fn token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == COOKIE).then_some(value)
        })
}

/// Counts a player as online for as long as their stream lives.
struct Connection {
    app: Arc<App>,
    id: PlayerId,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.app.game().disconnect(self.id, now_ms());
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    /// Version of the state the page was rendered with.
    seen: Option<String>,
}

async fn events(
    State(app): State<Arc<App>>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Response {
    let now = now_ms();
    let joined = {
        let mut game = app.game();
        let joined = game.join(token(&headers), now);
        if let Some((id, _)) = joined {
            game.connect(id, now);
        }
        joined
    };
    let Some((id, new_token)) = joined else {
        return full();
    };
    let conn = Connection {
        app: app.clone(),
        id,
    };
    let changes = app.changes.subscribe();
    // Skip the first event when the page already shows the current state:
    // re-rendering it could undo a pick whose request is still in flight.
    // After a phase change or a reconnect to a restarted server the versions
    // differ and the stream starts with a fresh view.
    let current = app.version(*changes.borrow());
    let send_now = query.seen.as_deref() != Some(current.as_str());
    let updates = stream::unfold(
        (conn, changes, send_now),
        |(conn, mut changes, first)| async move {
            if !first && changes.changed().await.is_err() {
                return None;
            }
            let fragment = html::game(&conn.app.game().view(conn.id, now_ms())).into_string();
            Some((
                Ok::<_, Infallible>(Event::default().data(fragment)),
                (conn, changes, false),
            ))
        },
    );
    let mut response = Sse::new(updates)
        .keep_alive(KeepAlive::default())
        .into_response();
    // nginx would otherwise hold events back until its buffer fills.
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    // The page load normally issued the cookie already; this covers a
    // server restart while the page stays open.
    if let Some(token) = new_token {
        set_cookie(&mut response, &token);
    }
    response
}

/// How often the spectator screen's answer count is refreshed.
const TALLY_EVERY: Duration = Duration::from_millis(500);

/// The spectator screen's stream: the whole view on every phase change, and
/// in between a `tally` event whenever the number of answers or players
/// changes.
async fn spectate_events(State(app): State<Arc<App>>) -> Response {
    let changes = app.changes.subscribe();
    let updates = stream::unfold(
        (app, changes, None::<Tally>),
        |(app, mut changes, mut shown)| async move {
            loop {
                let Some(last) = shown else {
                    let (fragment, tally) = {
                        let game = app.game();
                        let view = game.spectate(now_ms());
                        (
                            html::spectate_game(&view, game.tally()).into_string(),
                            game.tally(),
                        )
                    };
                    let event = Event::default().data(fragment);
                    return Some((Ok::<_, Infallible>(event), (app, changes, Some(tally))));
                };
                tokio::select! {
                    changed = changes.changed() => {
                        changed.ok()?;
                        shown = None;
                    }
                    () = tokio::time::sleep(TALLY_EVERY) => {
                        let (tally, text) = {
                            let game = app.game();
                            let tally = game.tally();
                            (tally, html::tally_text(tally, &game.spectate(0).phase))
                        };
                        if tally != last {
                            let event = Event::default().event("tally").data(text);
                            return Some((Ok(event), (app, changes, Some(tally))));
                        }
                    }
                }
            }
        },
    );
    let mut response = Sse::new(updates)
        .keep_alive(KeepAlive::default())
        .into_response();
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

/// The answer form: which question, and the index of the picked choice.
#[derive(Deserialize)]
struct AnswerRequest {
    seq: u64,
    choice: usize,
}

async fn answer(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Form(request): Form<AnswerRequest>,
) -> Response {
    let mut game = app.game();
    let Some(id) = token(&headers).and_then(|t| game.player_for(t)) else {
        return (StatusCode::UNAUTHORIZED, "unknown player").into_response();
    };
    match game.answer(id, request.seq, request.choice, now_ms()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(AnswerError::Closed) => (StatusCode::CONFLICT, "question closed").into_response(),
        Err(AnswerError::InvalidChoice) => {
            (StatusCode::BAD_REQUEST, "no such choice").into_response()
        }
    }
}
