//! Fehler der KI – mit verständlichen deutschen Meldungen und passendem HTTP-Status:
//! nicht eingerichtet (503), Rate-Limit (429), Anbieter-Fehler (502). Alles andere läuft
//! als [`AppError`] weiter.

use crate::error::AppError;
use rocket::http::{ContentType, Status};
use rocket::request::Request;
use rocket::response::{self, Responder, Response};
use std::io::Cursor;

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// KI ausgeschaltet oder unvollständig eingerichtet (kein Schlüssel, keine Adresse, kein Modell)
    #[error("{0}")]
    Unavailable(String),
    /// Schlüssel fehlt beim Anbieter oder wurde abgelehnt
    #[error("{0}")]
    KeyRejected(String),
    /// Das Modell kennt der Anbieter nicht
    #[error("{0}")]
    UnknownModel(String),
    /// Zu viele Anfragen bzw. Kontingent erschöpft
    #[error("{0}")]
    RateLimited(String),
    /// Anbieter nicht erreichbar oder Zeitüberschreitung
    #[error("{0}")]
    Unreachable(String),
    /// Sonstiger Fehler des Anbieters (HTTP 5xx, leere oder unbrauchbare Antwort)
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    App(#[from] AppError),
}

impl From<sqlx::Error> for AiError {
    fn from(e: sqlx::Error) -> Self {
        AiError::App(AppError::Database(e))
    }
}

pub type AiResult<T> = Result<T, AiError>;

impl AiError {
    /// Lohnt es sich, ein anderes Modell zu probieren? (nicht bei falschem Schlüssel)
    pub fn try_next_model(&self) -> bool {
        matches!(
            self,
            AiError::UnknownModel(_) | AiError::RateLimited(_) | AiError::Failed(_)
        )
    }

    fn status_and_code(&self) -> (Status, &'static str) {
        match self {
            AiError::Unavailable(_) => (Status::ServiceUnavailable, "ai_unavailable"),
            AiError::KeyRejected(_) => (Status::BadGateway, "ai_key_rejected"),
            AiError::UnknownModel(_) => (Status::BadGateway, "ai_model_unknown"),
            AiError::RateLimited(_) => (Status::TooManyRequests, "ai_rate_limited"),
            AiError::Unreachable(_) => (Status::BadGateway, "ai_unreachable"),
            AiError::Failed(_) => (Status::BadGateway, "ai_failed"),
            AiError::App(_) => (Status::InternalServerError, "internal_error"),
        }
    }
}

impl<'r> Responder<'r, 'static> for AiError {
    fn respond_to(self, req: &'r Request<'_>) -> response::Result<'static> {
        if let AiError::App(e) = self {
            return e.respond_to(req);
        }
        let (status, code) = self.status_and_code();
        // Nur Art des Fehlers und Anbieter – nie Inhalte von Nachrichten
        tracing::info!(path = %req.uri().path(), code, "KI: {self}");
        let body = serde_json::json!({ "error": { "code": code, "message": self.to_string() } })
            .to_string();
        Response::build()
            .status(status)
            .header(ContentType::JSON)
            .sized_body(body.len(), Cursor::new(body))
            .ok()
    }
}
