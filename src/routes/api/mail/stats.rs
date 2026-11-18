//! Statistik: `/stats`.

use crate::api::{self, StatsQuery};
use crate::auth::guards::CurrentUser;
use crate::error::AppResult;
use crate::mail::{accounts, stats};
use rocket::serde::json::Json;
use rocket::{get, routes, Route, State};
use sqlx::SqlitePool;

pub fn routes() -> Vec<Route> {
    routes![get_stats]
}

#[get("/stats?<account>&<days>&<tz>")]
async fn get_stats(
    user: CurrentUser,
    account: Option<&str>,
    days: Option<u32>,
    tz: Option<i32>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::Stats>> {
    let account = account.filter(|a| !a.is_empty()).map(String::from);
    if let Some(a) = &account {
        accounts::get(pool, &user.0.id, a).await?;
    }
    let q = StatsQuery {
        account,
        days: days.unwrap_or(0),
        tz: tz.unwrap_or(0),
    };
    Ok(Json(stats::stats(pool, &user.0.id, &q).await?))
}
