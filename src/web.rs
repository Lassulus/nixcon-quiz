//! HTTP surface: the server-rendered page, one Server-Sent Events stream per
//! player carrying HTML fragments of their view, and one form endpoint to
//! submit an answer.
//!
//! There are two independent games ("rooms"): the normal quiz at `/`, and a
//! fast one at `/fast` that closes each question as soon as everyone online
//! has answered. Each has its own players, clock, spectator screen and
//! cookie; the stylesheet, scripts and break slides are shared.
//!
//! Players are identified by an opaque random token in an HttpOnly cookie,
//! issued on the first page load or event stream without a known one.

use crate::{
    game::{AnswerError, Game, PlayerId, Tally},
    html::{self, Urls},
    slides::{self, Library, Slide},
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

/// Long enough to cover a whole conference.
const COOKIE_MAX_AGE: u32 = 14 * 24 * 60 * 60;

/// The normal quiz. Event streams stay under `/api/events` so the proxy
/// config for unbuffered streams covers both rooms.
const NORMAL: Urls = Urls {
    page: "/",
    spectate: "/spectate",
    qr: "/qr.svg",
    events: "/api/events",
    spectate_events: "/api/events/spectate",
    answer: "/api/answer",
};

const FAST: Urls = Urls {
    page: "/fast",
    spectate: "/fast/spectate",
    qr: "/fast/qr.svg",
    events: "/api/events/fast",
    spectate_events: "/api/events/fast/spectate",
    answer: "/api/answer/fast",
};

/// What the rooms share.
struct App {
    /// Random per process, so versions from before a restart never match.
    boot: u64,
    /// Where players join, for the QR code on the spectator screen.
    public_url: Option<String>,
    /// The spectator screen's break slides, when there is a slides directory.
    slides: Option<watch::Sender<Slides>>,
}

#[derive(Default)]
struct Slides {
    /// Everything from the last scan; any of these may be requested.
    all: Vec<Arc<Slide>>,
    shown: Option<Arc<Slide>>,
}

/// One game and where it lives.
struct Room {
    game: Mutex<Game>,
    /// Bumped whenever the phase changes; every event stream re-renders.
    changes: watch::Sender<u64>,
    urls: Urls,
    /// A cookie per room, so playing both keeps both players.
    cookie: &'static str,
}

impl Room {
    fn game(&self) -> std::sync::MutexGuard<'_, Game> {
        // A panic while holding the lock leaves the game in whatever state it
        // reached; carrying on beats taking the whole quiz down mid-talk.
        self.game.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Moves the game along if it's due, and tells every stream. Called with
    /// the game locked, so versions always match what was rendered.
    fn advance(&self, game: &mut Game, now: u64) {
        if game.tick(now) {
            self.changes.send_modify(|n| *n += 1);
        }
    }
}

/// A room's handlers need the room and what the rooms share.
#[derive(Clone)]
struct At {
    app: Arc<App>,
    room: Arc<Room>,
}

impl App {
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

/// `normal` is played at `/`, `fast` at `/fast`. `public_url` is the address
/// shown to the livestream audience; without it the spectator screen uses
/// the host it was loaded from. `slides` are shown next to the game on that
/// screen, each for the given time.
pub async fn serve(
    listener: TcpListener,
    normal: Game,
    fast: Game,
    public_url: Option<String>,
    slides: Option<(Library, Duration)>,
) -> std::io::Result<()> {
    let app = Arc::new(App {
        boot: rand::random(),
        public_url,
        slides: slides
            .is_some()
            .then(|| watch::Sender::new(Slides::default())),
    });
    if let Some((library, every)) = slides {
        tokio::spawn(run_slides(app.clone(), library, every));
    }
    let mut router = Router::new()
        .route("/clock.js", get(clock))
        .route("/style.css", get(style))
        .route("/vendor/{file}", get(vendor))
        .route("/slides/{file}", get(slide))
        .with_state(app.clone());
    for (game, urls, cookie) in [
        (normal, NORMAL, "nixcon_quiz"),
        (fast, FAST, "nixcon_quiz_fast"),
    ] {
        let room = Arc::new(Room {
            game: Mutex::new(game),
            changes: watch::Sender::new(0),
            urls,
            cookie,
        });
        tokio::spawn(run_clock(room.clone()));
        let at = At {
            app: app.clone(),
            room: room.clone(),
        };
        router = router.merge(
            Router::new()
                .route(room.urls.page, get(index))
                .route(room.urls.spectate, get(spectate))
                .route(room.urls.qr, get(qr))
                .route(room.urls.events, get(events))
                .route(room.urls.spectate_events, get(spectate_events))
                .route(room.urls.answer, post(answer))
                .with_state(at),
        );
    }
    axum::serve(listener, router).await
}

/// Moves the game along at each deadline and wakes every stream. Also wakes
/// on every phase change, since a fast room can close a question early and
/// bring the next deadline forward.
async fn run_clock(room: Arc<Room>) {
    let mut changes = room.changes.subscribe();
    loop {
        let deadline = {
            let mut game = room.game();
            room.advance(&mut game, now_ms());
            game.deadline()
        };
        changes.borrow_and_update();
        let wait = deadline.saturating_sub(now_ms()).max(1);
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(wait)) => {}
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// Looks for new, changed and removed slides, then moves on to the next one.
async fn run_slides(app: Arc<App>, mut library: Library, every: Duration) {
    let Some(sender) = &app.slides else { return };
    loop {
        let all;
        (library, all) = tokio::task::spawn_blocking(move || {
            let all = library.scan();
            (library, all)
        })
        .await
        .expect("scanning slides");
        sender.send_if_modified(|slides| {
            let next = slides::next(&all, slides.shown.as_deref());
            let changed = next.as_ref().map(|s| &s.name) != slides.shown.as_ref().map(|s| &s.name);
            *slides = Slides { all, shown: next };
            changed
        });
        tokio::time::sleep(every).await;
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
        "press-start-2p-latin-400-normal.woff2" => {
            (WOFF2, embed!("press-start-2p-latin-400-normal.woff2"))
        }
        "press-start-2p-latin-ext-400-normal.woff2" => {
            (WOFF2, embed!("press-start-2p-latin-ext-400-normal.woff2"))
        }
        "nixcon-2026-icon.svg" => ("image/svg+xml", embed!("nixcon-2026-icon.svg")),
        "nixcon-2026-cloud.png" => ("image/png", embed!("nixcon-2026-cloud.png")),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    static_file(content_type, "public, max-age=31536000, immutable", body)
}

/// Named after their content, so they may be cached forever too.
async fn slide(State(app): State<Arc<App>>, Path(file): Path<String>) -> Response {
    let found = app.slides.as_ref().and_then(|slides| {
        let slides = slides.borrow();
        slides.all.iter().find(|s| s.name == file).cloned()
    });
    let Some(slide) = found else {
        return StatusCode::NOT_FOUND.into_response();
    };
    static_file(
        slide.content_type,
        "public, max-age=31536000, immutable",
        slide.body.clone(),
    )
}

fn set_cookie(response: &mut Response, name: &str, token: &str) {
    let cookie =
        format!("{name}={token}; Path=/; Max-Age={COOKIE_MAX_AGE}; HttpOnly; SameSite=Strict");
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

async fn index(State(At { app, room }): State<At>, headers: HeaderMap) -> Response {
    let now = now_ms();
    let (page, new_token) = {
        let mut game = room.game();
        let Some((id, new_token)) = game.join(token(&headers, room.cookie), now) else {
            return full();
        };
        // The changes counter is only bumped with the game locked, so this
        // version matches the view exactly.
        let seen = app.version(*room.changes.borrow());
        let page = html::page(&game.view(id, now), &room.urls, &seen).into_string();
        (page, new_token)
    };
    let mut response = html_response(page);
    if let Some(token) = new_token {
        set_cookie(&mut response, room.cookie, &token);
    }
    response
}

/// Where players join the room: under the public URL, or the host this
/// request came to.
fn join_url(app: &App, room: &Room, headers: &HeaderMap) -> String {
    let base = app.public_url.clone().unwrap_or_else(|| {
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("localhost");
        format!("http://{host}/")
    });
    format!("{}{}", base.trim_end_matches('/'), room.urls.page)
}

/// The livestream screen. Watching doesn't make you a player: no cookie, and
/// it doesn't count as online.
async fn spectate(State(At { app, room }): State<At>, headers: HeaderMap) -> Response {
    let join = join_url(&app, &room, &headers);
    let slide = app.slides.as_ref().map(|s| s.borrow().shown.clone());
    let game = room.game();
    let page = html::spectate_page(
        &game.spectate(now_ms()),
        game.tally(),
        &room.urls,
        &join,
        slide.as_ref().map(Option::as_deref),
    )
    .into_string();
    html_response(page)
}

/// The spectator screen's QR code, as an image of its own.
async fn qr(State(At { app, room }): State<At>, headers: HeaderMap) -> Response {
    match html::qr_svg(&join_url(&app, &room, &headers)) {
        Some(svg) => static_file("image/svg+xml", "no-cache", svg),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn token<'a>(headers: &'a HeaderMap, cookie: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == cookie).then_some(value)
        })
}

/// Counts a player as online for as long as their stream lives.
struct Connection {
    room: Arc<Room>,
    id: PlayerId,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let now = now_ms();
        let mut game = self.room.game();
        game.disconnect(self.id, now);
        // In a fast room the last one who hadn't answered may just have left.
        self.room.advance(&mut game, now);
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    /// Version of the state the page was rendered with.
    seen: Option<String>,
}

async fn events(
    State(At { app, room }): State<At>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Response {
    let now = now_ms();
    let joined = {
        let mut game = room.game();
        let joined = game.join(token(&headers, room.cookie), now);
        if let Some((id, _)) = joined {
            game.connect(id, now);
        }
        joined
    };
    let Some((id, new_token)) = joined else {
        return full();
    };
    let conn = Connection {
        room: room.clone(),
        id,
    };
    let changes = room.changes.subscribe();
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
            let fragment = {
                let game = conn.room.game();
                html::game(&game.view(conn.id, now_ms()), &conn.room.urls).into_string()
            };
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
        set_cookie(&mut response, room.cookie, &token);
    }
    response
}

/// How often the spectator screen's answer count is refreshed.
const TALLY_EVERY: Duration = Duration::from_millis(500);

/// The spectator screen's stream: the whole view on every phase change, in
/// between a `tally` event whenever the number of answers or players
/// changes, and a `slide` event with each new break slide.
async fn spectate_events(State(At { app, room }): State<At>) -> Response {
    let changes = room.changes.subscribe();
    let slides = app.slides.as_ref().map(|s| {
        let mut slides = s.subscribe();
        // Start with the current slide: the stream may be a reconnect.
        slides.mark_changed();
        slides
    });
    let updates = stream::unfold(
        (room, changes, slides, None::<Tally>),
        |(room, mut changes, mut slides, mut shown)| async move {
            loop {
                let Some(last) = shown else {
                    let (fragment, tally) = {
                        let game = room.game();
                        let view = game.spectate(now_ms());
                        (
                            html::spectate_game(&view, game.tally()).into_string(),
                            game.tally(),
                        )
                    };
                    let event = Event::default().data(fragment);
                    return Some((
                        Ok::<_, Infallible>(event),
                        (room, changes, slides, Some(tally)),
                    ));
                };
                tokio::select! {
                    changed = changes.changed() => {
                        changed.ok()?;
                        shown = None;
                    }
                    Some(changed) = async { Some(slides.as_mut()?.changed().await) } => {
                        changed.ok()?;
                        let fragment = slides
                            .as_mut()
                            .map(|s| html::slide(s.borrow_and_update().shown.as_deref()).into_string())
                            .unwrap_or_default();
                        let event = Event::default().event("slide").data(fragment);
                        return Some((Ok(event), (room, changes, slides, shown)));
                    }
                    () = tokio::time::sleep(TALLY_EVERY) => {
                        let (tally, text) = {
                            let game = room.game();
                            let tally = game.tally();
                            (tally, html::tally_text(tally, &game.spectate(0).phase))
                        };
                        if tally != last {
                            let event = Event::default().event("tally").data(text);
                            return Some((Ok(event), (room, changes, slides, Some(tally))));
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
    State(At { room, .. }): State<At>,
    headers: HeaderMap,
    Form(request): Form<AnswerRequest>,
) -> Response {
    let now = now_ms();
    let mut game = room.game();
    let Some(id) = token(&headers, room.cookie).and_then(|t| game.player_for(t)) else {
        return (StatusCode::UNAUTHORIZED, "unknown player").into_response();
    };
    let result = game.answer(id, request.seq, request.choice, now);
    // In a fast room this may have been the last answer missing.
    room.advance(&mut game, now);
    match result {
        // What the pick earns if it's right, so the page can show it.
        Ok(points) => (StatusCode::NO_CONTENT, [("x-points", points.to_string())]).into_response(),
        Err(AnswerError::Closed) => (StatusCode::CONFLICT, "question closed").into_response(),
        Err(AnswerError::InvalidChoice) => {
            (StatusCode::BAD_REQUEST, "no such choice").into_response()
        }
    }
}
