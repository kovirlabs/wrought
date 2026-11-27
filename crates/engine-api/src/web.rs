//! The `/api/v1/` routes.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use engine_auth::{Access, Action, Auth, FieldAccess, User};
use engine_content::store::{Changes, Item, Key, ListQuery, SaveError, Status};
use engine_content::types::TypeDef;
use engine_content::{ContentSite, Store};
use engine_core::web::Client;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

use crate::ApiConfig;
use crate::limit::Limiter;
use crate::tokens::{self, Token, TokenStore};

/// An error answer: `{"error": {"status", "code", "message", ...}}`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub extra: Map<String, Json>,
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            extra: Map::new(),
            retry_after: None,
        }
    }

    fn with(mut self, k: &str, v: Json) -> Self {
        self.extra.insert(k.into(), v);
        self
    }

    /// A failure: `503` with `Retry-After` if its cause is that the
    /// database is unavailable (by the error's chain), else `500`.
    fn internal(e: impl Into<anyhow::Error>) -> Self {
        let e = e.into();
        eprintln!("api: {e:#}");
        if engine_db::health::is_unavailable_any(&e) {
            let mut r = ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "The database is unavailable; try again shortly.",
            );
            r.retry_after = Some(30);
            return r;
        }
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "The server could not finish this request.",
        )
    }

    fn not_found() -> Self {
        ApiError::new(StatusCode::NOT_FOUND, "not_found", "No such item.")
    }

    fn forbidden(what: &str) -> Self {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            format!("This token or its account may not {what}."),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = Map::new();
        body.insert("status".into(), json!(self.status.as_u16()));
        body.insert("code".into(), json!(self.code));
        body.insert("message".into(), json!(self.message));
        body.extend(self.extra);
        let mut r = json_response(self.status, &json!({ "error": body }));
        if self.status == StatusCode::UNAUTHORIZED {
            r.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"api\""),
            );
        }
        if let Some(s) = self.retry_after
            && let Ok(v) = HeaderValue::from_str(&s.to_string())
        {
            r.headers_mut().insert(header::RETRY_AFTER, v);
        }
        r
    }
}

fn json_response(status: StatusCode, v: &Json) -> Response {
    let mut r = (
        status,
        serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
    )
        .into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r
}

type ApiResult = Result<Response, ApiError>;

struct Inner {
    tokens: TokenStore,
    auth: Auth,
    store: Store,
    site: ContentSite,
    cfg: ApiConfig,
    per_token: Limiter,
    bad: Limiter,
}

#[derive(Clone)]
pub struct Api {
    inner: Arc<Inner>,
}

impl Api {
    pub fn new(auth: Auth, store: Store, site: ContentSite, cfg: ApiConfig) -> Self {
        let tokens = TokenStore::new(auth.db().clone());
        Api {
            inner: Arc::new(Inner {
                tokens,
                auth,
                store,
                site,
                per_token: Limiter::per_minute(cfg.requests_per_minute),
                bad: Limiter::per_minute(cfg.bad_tokens_per_minute),
                cfg,
            }),
        }
    }

    pub fn tokens(&self) -> &TokenStore {
        &self.inner.tokens
    }
}

/// Who is calling.
struct Caller {
    token: Token,
    user: User,
    access: Access,
}

impl Caller {
    fn can(&self, ty: &str, a: Action) -> bool {
        tokens::can(&self.access, &self.token, ty, a)
    }

    fn author(&self) -> String {
        format!("{} (API token {}…)", self.user.email, self.token.prefix)
    }
}

async fn caller(api: &Api, headers: &HeaderMap, client: &Client) -> Result<Caller, ApiError> {
    let i = &api.inner;
    let ip = client
        .ip(i.cfg.behind_proxy)
        .unwrap_or_else(|| "unknown".into());
    let secret = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let bad = |msg: &str| -> ApiError {
        match i.bad.hit(&format!("ip:{ip}")) {
            Err(wait) => {
                let mut e = ApiError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    "Too many requests without a valid token. Wait and try again.",
                );
                e.retry_after = Some(wait);
                e
            }
            Ok(()) => ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", msg),
        }
    };
    let Some(secret) = secret else {
        return Err(bad(
            "Send an API token: `Authorization: Bearer wr_...` (made in the admin).",
        ));
    };
    let token = match i.tokens.verify(secret).await {
        Ok(Some(t)) => t,
        Ok(None) => return Err(bad("The token is not valid: unknown, revoked or expired.")),
        Err(e) => return Err(ApiError::internal(e)),
    };
    if let Err(wait) = i.per_token.hit(&format!("token:{}", token.id)) {
        let mut e = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            format!(
                "This token made more than {} requests in a minute.",
                i.cfg.requests_per_minute
            ),
        );
        e.retry_after = Some(wait);
        return Err(e);
    }
    let user = match i.auth.user(token.user_id).await {
        Ok(Some(u)) if u.active => u,
        Ok(_) => return Err(bad("The token's account is blocked.")),
        Err(e) => return Err(ApiError::internal(e)),
    };
    let access = i.auth.access(&user).await.map_err(ApiError::internal)?;
    Ok(Caller {
        token,
        user,
        access,
    })
}

fn type_of<'a>(api: &'a Api, c: &Caller, ty: &str, a: Action) -> Result<&'a TypeDef, ApiError> {
    let Some(t) = api.inner.store.registry().get(ty) else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_type",
            format!("There is no content type `{ty}`."),
        ));
    };
    // A type the caller may not even view does not exist for them.
    if !c.can(&t.name, Action::View) {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_type",
            format!("There is no content type `{ty}`."),
        ));
    }
    if !c.can(&t.name, a) {
        let what = match a {
            Action::Create => "create",
            Action::Edit => "change",
            Action::Delete => "delete",
            Action::Publish => "publish",
            Action::View => "read",
        };
        return Err(ApiError::forbidden(&format!("{what} `{}` items", t.name)));
    }
    Ok(t)
}

/// An item as JSON, without the fields the caller may not read.
fn item_json(api: &Api, c: &Caller, t: &TypeDef, item: &Item) -> Json {
    let mut fields = Map::new();
    for (name, v) in item.input_fields(t) {
        if c.access.field(&t.name, &name, Action::View) != FieldAccess::None {
            fields.insert(name, v);
        }
    }
    json!({
        "type": t.name,
        "id": item.id,
        "slug": item.slug,
        "status": item.status.as_str(),
        "version": item.version,
        "created_at": item.created_at,
        "updated_at": item.updated_at,
        "published_at": item.published_at,
        "url": api.inner.site.item_path(item),
        "fields": fields,
    })
}

fn with_etag(mut r: Response, version: i32) -> Response {
    if let Ok(v) = HeaderValue::from_str(&format!("\"{version}\"")) {
        r.headers_mut().insert(header::ETAG, v);
    }
    r
}

// ----- routes -------------------------------------------------------------------

async fn index(State(api): State<Api>, headers: HeaderMap, client: Client) -> ApiResult {
    let c = caller(&api, &headers, &client).await?;
    let types: Vec<Json> = api
        .inner
        .store
        .registry()
        .types()
        .filter(|t| c.can(&t.name, Action::View))
        .map(|t| {
            json!({
                "name": t.name, "label": t.label, "singleton": t.singleton,
                "url": format!("/api/v1/content/{}/", t.name),
                "can": {
                    "create": c.can(&t.name, Action::Create),
                    "edit": c.can(&t.name, Action::Edit),
                    "delete": c.can(&t.name, Action::Delete),
                    "publish": c.can(&t.name, Action::Publish),
                },
            })
        })
        .collect();
    Ok(json_response(
        StatusCode::OK,
        &json!({
            "account": { "id": c.user.id, "email": c.user.email },
            "token": { "name": c.token.name, "scopes": c.token.scopes, "expires_at": c.token.expires_at },
            "types": types,
        }),
    ))
}

#[derive(Deserialize)]
struct ListQ {
    page: Option<i64>,
    per_page: Option<i64>,
    status: Option<String>,
    order: Option<String>,
    term: Option<String>,
    q: Option<String>,
}

async fn list(
    State(api): State<Api>,
    headers: HeaderMap,
    client: Client,
    Path(ty): Path<String>,
    q: Result<Query<ListQ>, axum::extract::rejection::QueryRejection>,
) -> ApiResult {
    let c = caller(&api, &headers, &client).await?;
    let t = type_of(&api, &c, &ty, Action::View)?.clone();
    let Query(q) =
        q.map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "bad_query", e.body_text()))?;
    let per_page = q.per_page.unwrap_or(20);
    if !(1..=100).contains(&per_page) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_query",
            "per_page must be between 1 and 100",
        ));
    }
    let status = match q.status.as_deref().unwrap_or("published") {
        "published" => Some(Status::Published),
        "draft" => Some(Status::Draft),
        "all" => None,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_query",
                "status is published, draft or all",
            ));
        }
    };
    let mut lq = ListQuery {
        status,
        limit: Some(per_page),
        offset: 0,
        order: q.order.clone().unwrap_or_else(|| "newest".into()),
        term: q.term.clone(),
        search: q.q.clone(),
    };
    let store = &api.inner.store;
    let total = store
        .count(&t.name, &lq)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "bad_query", format!("{e:#}")))?;
    let pages = ((total + per_page - 1) / per_page).max(1);
    let page = q.page.unwrap_or(1).clamp(1, pages);
    lq.offset = (page - 1) * per_page;
    let items = store.list(&t.name, &lq).await.map_err(ApiError::internal)?;
    let url = |p: i64| {
        let mut s = format!("/api/v1/content/{}/?page={p}&per_page={per_page}", t.name);
        if let Some(st) = &q.status {
            s.push_str(&format!("&status={st}"));
        }
        s
    };
    let next = (page < pages).then(|| url(page + 1));
    let prev = (page > 1).then(|| url(page - 1));
    let data: Vec<Json> = items.iter().map(|i| item_json(&api, &c, &t, i)).collect();
    let mut r = json_response(
        StatusCode::OK,
        &json!({
            "data": data, "page": page, "per_page": per_page, "total": total, "pages": pages,
            "next": next, "prev": prev,
        }),
    );
    let mut links = Vec::new();
    if let Some(n) = &next {
        links.push(format!("<{n}>; rel=\"next\""));
    }
    if let Some(p) = &prev {
        links.push(format!("<{p}>; rel=\"prev\""));
    }
    if !links.is_empty()
        && let Ok(v) = HeaderValue::from_str(&links.join(", "))
    {
        r.headers_mut().insert(header::LINK, v);
    }
    Ok(r)
}

async fn find(api: &Api, t: &TypeDef, key: &str) -> Result<Item, ApiError> {
    api.inner
        .store
        .get(&t.name, &Key::parse(key), None)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(ApiError::not_found)
}

async fn get_one(
    State(api): State<Api>,
    headers: HeaderMap,
    client: Client,
    Path((ty, key)): Path<(String, String)>,
) -> ApiResult {
    let c = caller(&api, &headers, &client).await?;
    let t = type_of(&api, &c, &ty, Action::View)?.clone();
    let item = find(&api, &t, &key).await?;
    Ok(with_etag(
        json_response(StatusCode::OK, &item_json(&api, &c, &t, &item)),
        item.version,
    ))
}

/// A write's body: `{"fields": {...}, "slug": ..., "status": ..., "version": ...}`.
struct Body {
    fields: Map<String, Json>,
    slug: Option<String>,
    status: Option<Status>,
    version: Option<i32>,
}

fn parse_body(bytes: &[u8]) -> Result<Body, ApiError> {
    let bad = |m: String| ApiError::new(StatusCode::BAD_REQUEST, "bad_json", m);
    let v: Json =
        serde_json::from_slice(bytes).map_err(|e| bad(format!("The body is not JSON: {e}")))?;
    let Json::Object(mut m) = v else {
        return Err(bad("The body must be a JSON object".into()));
    };
    let fields = match m.remove("fields") {
        None => Map::new(),
        Some(Json::Object(f)) => f,
        Some(_) => return Err(bad("`fields` must be an object".into())),
    };
    let slug = match m.remove("slug") {
        None | Some(Json::Null) => None,
        Some(Json::String(s)) => Some(s),
        Some(_) => return Err(bad("`slug` must be text".into())),
    };
    let status = match m.remove("status") {
        None | Some(Json::Null) => None,
        Some(Json::String(s)) => {
            Some(Status::parse(&s).ok_or_else(|| bad("`status` is draft or published".into()))?)
        }
        Some(_) => return Err(bad("`status` is draft or published".into())),
    };
    let version = match m.remove("version") {
        None | Some(Json::Null) => None,
        Some(v) => Some(
            v.as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| bad("`version` must be a number".into()))?,
        ),
    };
    if let Some(k) = m.keys().next() {
        return Err(bad(format!(
            "unknown key `{k}`: the body has `fields`, `slug`, `status` and `version`"
        )));
    }
    Ok(Body {
        fields,
        slug,
        status,
        version,
    })
}

/// The version a change is based on: `If-Match: "3"` (or `3`), else the
/// body's `version`.
fn expected_version(headers: &HeaderMap, body: Option<i32>) -> Result<i32, ApiError> {
    if let Some(v) = headers.get(header::IF_MATCH) {
        let s = v.to_str().unwrap_or("").trim().trim_start_matches("W/");
        return s.trim_matches('"').parse::<i32>().map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_version",
                "If-Match must be the item's version, e.g. \"3\" (its ETag)",
            )
        });
    }
    body.ok_or_else(|| {
        ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "version_required",
            "Say which version you changed: send `If-Match: \"<version>\"` (the ETag of the item) \
             or `\"version\"` in the body. This keeps two writers from overwriting each other.",
        )
    })
}

/// Refuse fields the caller may not write; build the changes.
fn changes(c: &Caller, t: &TypeDef, b: Body, a: Action) -> Result<Changes, ApiError> {
    let blocked: Vec<String> = b
        .fields
        .keys()
        .filter(|k| t.field(k).is_some())
        .filter(|k| c.access.field(&t.name, k, a) != FieldAccess::Write)
        .cloned()
        .collect();
    if !blocked.is_empty() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "field_forbidden",
            format!(
                "This account may not write: {}. Nothing was saved.",
                blocked.join(", ")
            ),
        )
        .with("fields", json!(blocked)));
    }
    if b.status.is_some() && !c.can(&t.name, Action::Publish) {
        return Err(ApiError::forbidden(&format!(
            "publish or unpublish `{}` items",
            t.name
        )));
    }
    Ok(Changes {
        fields: b.fields.into_iter().collect(),
        slug: b.slug,
        status: b.status,
    })
}

fn save_error(e: SaveError) -> ApiError {
    match e {
        SaveError::Invalid(errs) => {
            let mut fields = Map::new();
            for (k, v) in errs.0 {
                fields.insert(k, json!(v));
            }
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid",
                "Some values were not accepted. Nothing was saved.",
            )
            .with("fields", Json::Object(fields))
        }
        SaveError::Conflict {
            expected, current, ..
        } => ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            format!(
                "The item was changed by someone else: you sent version {expected}, it is now at \
                 version {current}. Read it again and apply your change to the new version."
            ),
        )
        .with("current_version", json!(current)),
        SaveError::NotFound { .. } => ApiError::not_found(),
        SaveError::Other(e) => ApiError::internal(e),
    }
}

async fn create(
    State(api): State<Api>,
    headers: HeaderMap,
    client: Client,
    Path(ty): Path<String>,
    body: Bytes,
) -> ApiResult {
    let c = caller(&api, &headers, &client).await?;
    let t = type_of(&api, &c, &ty, Action::Create)?.clone();
    let b = parse_body(&body)?;
    let ch = changes(&c, &t, b, Action::Create)?;
    let item = api
        .inner
        .store
        .create(&t.name, &ch, Some(&c.author()))
        .await
        .map_err(save_error)?;
    let mut r = with_etag(
        json_response(StatusCode::CREATED, &item_json(&api, &c, &t, &item)),
        item.version,
    );
    if let Ok(v) = HeaderValue::from_str(&format!("/api/v1/content/{}/{}", t.name, item.id)) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    Ok(r)
}

async fn update(
    State(api): State<Api>,
    headers: HeaderMap,
    client: Client,
    Path((ty, key)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let c = caller(&api, &headers, &client).await?;
    let t = type_of(&api, &c, &ty, Action::Edit)?.clone();
    let b = parse_body(&body)?;
    let expected = expected_version(&headers, b.version)?;
    let item = find(&api, &t, &key).await?;
    let ch = changes(&c, &t, b, Action::Edit)?;
    let saved = api
        .inner
        .store
        .update(&t.name, item.id, expected, &ch, Some(&c.author()))
        .await
        .map_err(save_error)?;
    Ok(with_etag(
        json_response(StatusCode::OK, &item_json(&api, &c, &t, &saved)),
        saved.version,
    ))
}

#[derive(Deserialize)]
struct VersionQ {
    version: Option<i32>,
}

async fn delete(
    State(api): State<Api>,
    headers: HeaderMap,
    client: Client,
    Path((ty, key)): Path<(String, String)>,
    Query(q): Query<VersionQ>,
) -> ApiResult {
    let c = caller(&api, &headers, &client).await?;
    let t = type_of(&api, &c, &ty, Action::Delete)?.clone();
    let expected = expected_version(&headers, q.version)?;
    let item = find(&api, &t, &key).await?;
    api.inner
        .store
        .delete(&t.name, item.id, Some(expected), Some(&c.author()))
        .await
        .map_err(save_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn fallback() -> Response {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "No such API endpoint.").into_response()
}

/// The routes, under `/api/v1/`.
pub fn router(api: Api) -> Router {
    Router::new()
        .route("/api/v1", get(|| async { redirect("/api/v1/") }))
        .route("/api/v1/", get(index))
        .route("/api/v1/content/{ty}/", get(list).post(create))
        .route(
            "/api/v1/content/{ty}/{key}",
            get(get_one).patch(update).delete(delete),
        )
        // Unknown API paths answer in JSON too (not the site's 404 page).
        .route("/api/{*rest}", axum::routing::any(fallback))
        .with_state(api)
}

fn redirect(to: &'static str) -> Response {
    let mut r = StatusCode::PERMANENT_REDIRECT.into_response();
    r.headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static(to));
    r
}

/// Map of `type name → label` the caller may read (for docs pages).
pub fn readable_types(store: &Store, access: &Access) -> BTreeMap<String, String> {
    store
        .registry()
        .types()
        .filter(|t| access.can(&t.name, Action::View))
        .map(|t| (t.name.clone(), t.label.clone()))
        .collect()
}
