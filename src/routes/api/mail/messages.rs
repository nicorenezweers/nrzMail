//! Nachrichten: Liste, Detail, Rohdaten, Unterhaltungen, Aktionen, Anhänge, Kontakte.

use super::{Body, Download};
use crate::api::{self, Category, MailView, MessageAction, MessageQuery};
use crate::auth::guards::CurrentUser;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::mail::{actions, archive, messages, pdf as pdf_writer, unsubscribe as unsub};
use crate::services::activity;
use rocket::http::ContentType;
use rocket::serde::json::Json;
use rocket::{get, post, routes, FromForm, Route, State};
use sqlx::SqlitePool;

pub fn routes() -> Vec<Route> {
    routes![list, detail, raw, pdf, thread, thread_pdf, unsubscribe, bulk, attachment, contacts]
}

/// Abfrage-Parameter von `GET /messages` und `GET /export`.
#[derive(Debug, Default, FromForm)]
pub struct QueryParams<'r> {
    pub view: Option<&'r str>,
    pub account: Option<&'r str>,
    pub folder: Option<&'r str>,
    pub category: Option<&'r str>,
    pub q: Option<&'r str>,
    pub unread: Option<bool>,
    pub flagged: Option<bool>,
    pub attachments: Option<bool>,
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub threads: Option<bool>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
    /// Nur für den Export
    pub format: Option<&'r str>,
    pub ids: Option<&'r str>,
}

impl QueryParams<'_> {
    pub fn to_query(&self) -> AppResult<MessageQuery> {
        let some = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        Ok(MessageQuery {
            view: match some(self.view) {
                Some(v) => Some(
                    MailView::parse(&v)
                        .ok_or_else(|| AppError::bad(format!("Unbekannte Ansicht „{v}“.")))?,
                ),
                None => None,
            },
            account: some(self.account),
            folder: some(self.folder),
            category: match some(self.category) {
                Some(c) => Some(
                    Category::parse(&c)
                        .ok_or_else(|| AppError::bad(format!("Unbekannte Kategorie „{c}“.")))?,
                ),
                None => None,
            },
            q: some(self.q),
            unread: self.unread,
            flagged: self.flagged,
            attachments: self.attachments,
            after: self.after,
            before: self.before,
            threads: self.threads,
            offset: self.offset,
            limit: self.limit,
        })
    }

    /// `ids=a,b,c`
    pub fn ids(&self) -> Option<Vec<String>> {
        self.ids.map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
    }
}

#[get("/messages?<p..>")]
async fn list(
    user: CurrentUser,
    p: QueryParams<'_>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::MessageList>> {
    Ok(Json(
        messages::list(pool, &user.0.id, &p.to_query()?).await?,
    ))
}

#[get("/messages/<id>")]
async fn detail(
    user: CurrentUser,
    id: &str,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::MessageDetail>> {
    Ok(Json(messages::detail(pool, &user.0.id, id).await?))
}

#[get("/messages/<id>/raw")]
async fn raw(user: CurrentUser, id: &str, pool: &State<SqlitePool>) -> AppResult<Download> {
    let (subject, raw) = messages::raw(pool, &user.0.id, id).await?;
    Ok(Download {
        body: Body::Bytes(raw),
        content_type: ContentType::new("message", "rfc822"),
        filename: format!("{}.eml", archive::safe_filename(&subject, 100)),
        inline: false,
    })
}

/// Nachricht als PDF. `tz` = Zeitzone des Browsers (Minuten östlich von UTC) für die Daten.
#[get("/messages/<id>/pdf?<tz>")]
async fn pdf(user: CurrentUser, id: &str, tz: Option<i32>, pool: &State<SqlitePool>) -> AppResult<Download> {
    let m = messages::detail(pool, &user.0.id, id).await?;
    let subject = m.summary.subject.clone();
    let bytes = pdf_writer::render(&subject, std::slice::from_ref(&m), tz.unwrap_or(0));
    Ok(Download {
        body: Body::Bytes(bytes),
        content_type: ContentType::PDF,
        filename: format!("{}.pdf", archive::safe_filename(&subject, 100)),
        inline: false,
    })
}

/// Ganze Unterhaltung als PDF (älteste zuerst).
#[get("/threads/<id>/pdf?<tz>")]
async fn thread_pdf(user: CurrentUser, id: &str, tz: Option<i32>, pool: &State<SqlitePool>) -> AppResult<Download> {
    let t = messages::thread(pool, &user.0.id, id).await?;
    let bytes = pdf_writer::render(&t.subject, &t.messages, tz.unwrap_or(0));
    Ok(Download {
        body: Body::Bytes(bytes),
        content_type: ContentType::PDF,
        filename: format!("{} (Unterhaltung).pdf", archive::safe_filename(&t.subject, 90)),
        inline: false,
    })
}

/// Vom Newsletter abmelden (automatisch, so weit es geht).
#[post("/messages/<id>/unsubscribe")]
async fn unsubscribe(
    user: CurrentUser,
    id: &str,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::UnsubscribeResult>> {
    let cleanup = user.0.preferences().unsubscribe_cleanup;
    let r = unsub::unsubscribe(pool, config, &user.0.id, id, cleanup).await?;
    activity::log(
        pool,
        Some(&user.0.id),
        "unsubscribe",
        &format!("{} ({})", if r.done { "abgemeldet" } else { "nicht automatisch" }, r.method),
    )
    .await;
    Ok(Json(r))
}

#[get("/threads/<id>")]
async fn thread(
    user: CurrentUser,
    id: &str,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::Thread>> {
    Ok(Json(messages::thread(pool, &user.0.id, id).await?))
}

#[post("/messages/actions", data = "<action>")]
async fn bulk(
    user: CurrentUser,
    action: Json<api::BulkAction>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::BulkResult>> {
    let result = actions::apply(pool, config, &user.0.id, &action).await?;
    if action.action == MessageAction::Delete && result.updated > 0 {
        activity::log(
            pool,
            Some(&user.0.id),
            "messages_deleted",
            &format!("{} Nachrichten endgültig gelöscht", result.updated),
        )
        .await;
    }
    Ok(Json(result))
}

/// Bilder, die der Browser direkt anzeigen darf (kein SVG – das kann Skripte enthalten).
fn displayable_image(ct: &str) -> bool {
    matches!(
        ct,
        "image/png"
            | "image/jpeg"
            | "image/jpg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/avif"
    )
}

#[get("/attachments/<id>?<inline>")]
async fn attachment(
    user: CurrentUser,
    id: &str,
    inline: Option<bool>,
    pool: &State<SqlitePool>,
) -> AppResult<Download> {
    let (att, data) = messages::attachment(pool, &user.0.id, id).await?;
    let ct = att.content_type.to_ascii_lowercase();
    let inline = inline == Some(true) && displayable_image(&ct);
    // Was im Browser Skripte ausführen könnte, kommt nur als Download
    let risky = ct.contains("html")
        || ct.contains("xml")
        || ct.contains("javascript")
        || ct.contains("svg");
    let content_type = if risky {
        ContentType::Binary
    } else {
        ContentType::parse_flexible(&ct).unwrap_or(ContentType::Binary)
    };
    Ok(Download {
        body: Body::Bytes(data),
        content_type,
        filename: archive::safe_filename(&att.filename, 200),
        inline,
    })
}

#[get("/contacts?<q>&<limit>")]
async fn contacts(
    user: CurrentUser,
    q: Option<&str>,
    limit: Option<i64>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<Vec<api::Contact>>> {
    Ok(Json(
        messages::contacts(pool, &user.0.id, q.unwrap_or(""), limit.unwrap_or(10)).await?,
    ))
}
