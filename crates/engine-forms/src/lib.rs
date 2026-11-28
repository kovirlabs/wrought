//! Hammerwright's `forms` feature: forms defined in `forms/*.toml` (or made in
//! the admin), shown on `request` pages with the Forms components,
//! validated on the server, stored, and guarded against spam.
//!
//! * [`def`]: form definitions; [`validate`]: server-side validation;
//! * [`store`]: admin-made forms, submissions, spam events;
//! * [`Forms`]: a site's forms; its [`SiteExtension`] adds the
//!   `form_view()` and `my_submissions()` template functions and the
//!   components `form`, `contact_form`, `newsletter_signup` and
//!   `multi_step_form`; [`web`] answers `POST /_engine/forms/<name>/`.
//!
//! **Spam guard** (no third-party captcha): a honeypot field people never
//! see; a time trap (a signed stamp of when the form was shown, so forms
//! sent faster than `[spam] min_seconds` or older than `max_age_hours` are
//! refused); and a rate limit per form and client address
//! (`max_per_hour`). Honeypot hits get the normal thank-you answer, so bots
//! learn nothing, but nothing is stored. Every refusal is logged in
//! `engine_form_events`.
//!
//! **CSRF:** every form carries `_csrf`: the session's token for signed-in
//! visitors, the double-submit cookie's for others; posts from other
//! origins are refused. Forms therefore need `request`-mode pages (the
//! token is made per visitor); `form_view()` refuses to render elsewhere.
//!
//! Built-in `contact` and `newsletter` forms back the components when a
//! site has no `forms/contact.toml` / `forms/newsletter.toml` of its own.
//! File upload fields (`type = "file"`, `media` feature) take one file each:
//! checked by its content like every upload, stored as a *private* media
//! file (never served at `/media/`), and recorded in the submission as
//! `{media_id, name, type, bytes}`; the admin links to it.

pub mod def;
pub mod outage;
pub mod spool;
pub mod store;
pub mod validate;
pub mod web;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use engine_core::builder::SiteExtension;
use engine_db::{Db, Migration};
use hmac::{Hmac, Mac};
use include_dir::{Dir, include_dir};
use minijinja::value::{Kwargs, Value};
use minijinja::{Error, ErrorKind, State};
use serde::Serialize;
use sha2::Sha256;

pub use def::{FormDef, Source};
pub use store::{FormStore, SaveError, Submission};

/// The `forms` feature's schema.
pub fn migrations() -> Vec<Migration> {
    vec![
        Migration::new(
            "forms/0001_forms",
            include_str!("../migrations/postgres/0001_forms.sql"),
        ),
        Migration::new(
            "forms/0002_form_spool",
            include_str!("../migrations/postgres/0002_form_spool.sql"),
        ),
    ]
}

/// The Forms components.
pub static COMPONENTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/site-templates");

/// The Forms components that need `media` (`file_upload`).
#[cfg(feature = "media")]
pub static MEDIA_COMPONENTS: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/site-templates-media");

/// The template folders of this build's Forms components.
pub fn component_dirs() -> Vec<&'static Dir<'static>> {
    #[allow(unused_mut)]
    let mut v: Vec<&'static Dir<'static>> = vec![&COMPONENTS];
    #[cfg(feature = "media")]
    v.push(&MEDIA_COMPONENTS);
    v
}

/// Built-in form definitions (used when the site has none of that name).
static BUILTIN_FORMS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/forms");

/// Built-in forms with file fields (`media` feature): `upload`.
#[cfg(feature = "media")]
static BUILTIN_MEDIA_FORMS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/forms-media");

fn builtin_form_files() -> impl Iterator<Item = &'static include_dir::File<'static>> {
    #[allow(unused_mut)]
    let mut v: Vec<&'static include_dir::File<'static>> = BUILTIN_FORMS.files().collect();
    #[cfg(feature = "media")]
    v.extend(BUILTIN_MEDIA_FORMS.files());
    v.into_iter()
}

/// A site's forms. Cheap to clone.
#[derive(Clone)]
pub struct Forms {
    inner: Arc<Inner>,
}

struct Inner {
    files: BTreeMap<String, FormDef>,
    store: Option<FormStore>,
    rt: tokio::runtime::Handle,
    stamp_key: Vec<u8>,
    /// Where posts wait while the database is unavailable.
    spool: std::sync::OnceLock<Arc<spool::Spool>>,
    /// The per-hour limit while the database cannot count (shared by the
    /// forms that take posts during the start and the full engine's).
    outage: std::sync::OnceLock<Arc<outage::Limiter>>,
    /// Taking posts before the database was reached ([`Forms::offline`]).
    offline: bool,
    #[cfg(feature = "media")]
    media: Option<engine_media::MediaStore>,
}

/// Where a form came from, with its stored version (admin forms).
#[derive(Debug, Clone)]
pub struct Found {
    pub def: FormDef,
    pub source: Source,
    pub version: i32,
}

/// Summary for lists.
#[derive(Debug, Clone, Serialize)]
pub struct Listed {
    pub name: String,
    pub title: String,
    pub source: Source,
    pub builtin: bool,
    pub version: i32,
    pub submissions: i64,
    pub spam_today: i64,
}

/// The file in the spool's folder that keeps the time-trap key.
const STAMP_KEY_FILE: &str = "stamp.key";

/// Read `forms/*.toml` of a site plus the built-in forms it does not
/// override.
pub fn load_files(site: &Path) -> Result<BTreeMap<String, FormDef>> {
    let mut out = BTreeMap::new();
    for f in builtin_form_files() {
        let name = f
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let text = f.contents_utf8().unwrap_or_default();
        out.insert(name.to_string(), FormDef::parse_toml(name, text)?);
    }
    let dir = site.join("forms");
    if dir.is_dir() {
        let mut names = Vec::new();
        for e in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "toml") {
                names.push(p);
            }
        }
        names.sort();
        for p in names {
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let text =
                std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
            out.insert(name.clone(), FormDef::parse_toml(&name, &text)?);
        }
    }
    Ok(out)
}

/// Whether `name` is one of the engine's built-in forms.
pub fn is_builtin(name: &str) -> bool {
    let file = format!("{name}.toml");
    builtin_form_files().any(|f| f.path().to_str() == Some(file.as_str()))
}

impl Forms {
    /// Load the site's form files and, with a database, the key that signs
    /// time-trap stamps. Without a database only file forms exist and
    /// nothing can be submitted (e.g. `engine build` of a site without
    /// one).
    pub async fn open(site: &Path, db: Option<Db>, rt: tokio::runtime::Handle) -> Result<Self> {
        let files = load_files(site)?;
        let store = db.map(FormStore::new);
        let stamp_key = match &store {
            Some(s) => s.engine_key("forms.stamp").await?,
            None => engine_core::web::secret::random_bytes(32),
        };
        Ok(Forms {
            inner: Arc::new(Inner {
                files,
                store,
                rt,
                stamp_key,
                spool: std::sync::OnceLock::new(),
                outage: std::sync::OnceLock::new(),
                offline: false,
                #[cfg(feature = "media")]
                media: None,
            }),
        })
    }

    /// The site's file forms (and the built-in ones), taking posts into
    /// `spool` before the database has been reached (DECISIONS.md, "Spool
    /// form posts during start"). Their stamps are checked with the key the
    /// full engine kept in the spool ([`Forms::remember_stamp_key`]);
    /// `None` until it has once (the first start ever): posts then get
    /// `503`, as before. Admin-made forms are defined in the database: a
    /// post to a name the files do not have gets `503`, not `404`.
    pub fn offline(
        site: &Path,
        spool: Arc<spool::Spool>,
        rt: tokio::runtime::Handle,
    ) -> Result<Option<Self>> {
        let Some(stamp_key) = spool.read_secret(STAMP_KEY_FILE) else {
            return Ok(None);
        };
        let files = load_files(site)?;
        let inner = Inner {
            files,
            store: None,
            rt,
            stamp_key,
            spool: std::sync::OnceLock::new(),
            outage: std::sync::OnceLock::new(),
            offline: true,
            #[cfg(feature = "media")]
            media: None,
        };
        let _ = inner.spool.set(spool);
        Ok(Some(Forms {
            inner: Arc::new(inner),
        }))
    }

    /// Whether these are the forms of [`Forms::offline`].
    pub fn is_offline(&self) -> bool {
        self.inner.offline
    }

    /// Keep the time-trap key in the spool's folder, so the forms of a
    /// later start can check stamps before the database is reached.
    pub fn remember_stamp_key(&self) -> Result<()> {
        match (self.spool(), &self.inner.store) {
            (Some(s), Some(_)) => s.write_secret(STAMP_KEY_FILE, &self.inner.stamp_key),
            _ => Ok(()),
        }
    }

    /// Count the per-hour limit of an outage together with `earlier`'s
    /// (the forms that took posts during the start). Call before serving.
    pub fn share_outage_limit(&self, earlier: &Forms) {
        let _ = self.inner.outage.set(earlier.outage_arc());
    }

    fn outage_arc(&self) -> Arc<outage::Limiter> {
        self.inner.outage.get_or_init(Default::default).clone()
    }

    /// Keep posts in `spool` while the database cannot take them (`engine
    /// serve`; set once).
    pub fn set_spool(&self, spool: Arc<spool::Spool>) {
        let _ = self.inner.spool.set(spool);
    }

    pub fn spool(&self) -> Option<&Arc<spool::Spool>> {
        self.inner.spool.get()
    }

    pub(crate) fn outage_limiter(&self) -> &outage::Limiter {
        self.inner.outage.get_or_init(Default::default)
    }

    /// Where file fields store their uploads (as private media).
    #[cfg(feature = "media")]
    pub fn with_media(mut self, media: engine_media::MediaStore) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("configure forms before cloning them")
            .media = Some(media);
        self
    }

    #[cfg(feature = "media")]
    pub fn media(&self) -> Option<&engine_media::MediaStore> {
        self.inner.media.as_ref()
    }

    pub fn store(&self) -> Option<&FormStore> {
        self.inner.store.as_ref()
    }

    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.inner.rt.block_on(f)
    }

    /// A form by name: the site's file (or the built-in one) wins over an
    /// admin form of the same name.
    pub async fn get(&self, name: &str) -> Result<Option<Found>> {
        if let Some(d) = self.inner.files.get(name) {
            return Ok(Some(Found {
                def: d.clone(),
                source: Source::File,
                version: 0,
            }));
        }
        let Some(store) = &self.inner.store else {
            return Ok(None);
        };
        if !def::valid_name(name) {
            return Ok(None);
        }
        match store.admin_form(name).await? {
            Some(f) => Ok(Some(Found {
                def: FormDef::from_json(name, &f.definition)?,
                source: Source::Admin,
                version: f.version,
            })),
            None => Ok(None),
        }
    }

    /// Every form, by name.
    pub async fn list(&self) -> Result<Vec<Listed>> {
        let (counts, spam) = match &self.inner.store {
            Some(s) => (
                s.counts().await?.into_iter().collect::<BTreeMap<_, _>>(),
                s.spam_counts()
                    .await?
                    .into_iter()
                    .collect::<BTreeMap<_, _>>(),
            ),
            None => Default::default(),
        };
        let mut out: BTreeMap<String, Listed> = BTreeMap::new();
        if let Some(s) = &self.inner.store {
            for f in s.admin_forms().await? {
                let title = f.definition["title"]
                    .as_str()
                    .unwrap_or(&f.name)
                    .to_string();
                out.insert(
                    f.name.clone(),
                    Listed {
                        name: f.name.clone(),
                        title,
                        source: Source::Admin,
                        builtin: false,
                        version: f.version,
                        submissions: 0,
                        spam_today: 0,
                    },
                );
            }
        }
        for (name, d) in &self.inner.files {
            out.insert(
                name.clone(),
                Listed {
                    name: name.clone(),
                    title: d.title.clone(),
                    source: Source::File,
                    builtin: is_builtin(name),
                    version: 0,
                    submissions: 0,
                    spam_today: 0,
                },
            );
        }
        for l in out.values_mut() {
            l.submissions = counts.get(&l.name).copied().unwrap_or(0);
            l.spam_today = spam.get(&l.name).copied().unwrap_or(0);
        }
        Ok(out.into_values().collect())
    }

    /// Whether a file (or built-in) form has this name.
    pub fn is_file_form(&self, name: &str) -> bool {
        self.inner.files.contains_key(name)
    }

    /// A signed stamp of now, for the time trap.
    pub fn stamp(&self, form: &str) -> String {
        let ms = now_ms();
        format!("{ms}.{}", self.sign(form, ms))
    }

    fn sign(&self, form: &str, ms: u128) -> String {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&self.inner.stamp_key).expect("any key length");
        mac.update(format!("{form}:{ms}").as_bytes());
        mac.finalize()
            .into_bytes()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Seconds since a stamp was made, if it is genuine.
    pub fn stamp_age(&self, form: &str, stamp: &str) -> Option<f64> {
        let (ms, sig) = stamp.split_once('.')?;
        let ms: u128 = ms.parse().ok()?;
        if !engine_core::web::secret::same(&self.sign(form, ms), sig) {
            return None;
        }
        let now = now_ms();
        (ms <= now + 5_000).then(|| (now.saturating_sub(ms)) as f64 / 1000.0)
    }

    /// The template extension (functions and components).
    pub fn extension(&self) -> Arc<dyn SiteExtension> {
        Arc::new(FormsExtension {
            forms: self.clone(),
        })
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

struct FormsExtension {
    forms: Forms,
}

fn mj(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::InvalidOperation, e.to_string())
}

fn lookup(state: &State, key: &str) -> Option<Value> {
    state
        .lookup(key)
        .filter(|v| !v.is_undefined() && !v.is_none())
}

impl FormsExtension {
    /// `form_view(name)`: everything the form components print.
    fn view(&self, state: &State, name: String, kwargs: Kwargs) -> Result<Value, Error> {
        kwargs.assert_all_used()?;
        let found = self
            .forms
            .block_on(self.forms.get(&name))
            .map_err(engine_core::templates::fn_error)?
            .ok_or_else(|| {
                mj(format!(
                    "form_view: no form `{name}` (add forms/{name}.toml)"
                ))
            })?;
        let def = found.def;
        let csrf = lookup(state, "csrf").and_then(|v| v.as_str().map(str::to_string));
        let request = lookup(state, "request");
        let (Some(csrf), Some(request)) = (csrf, request) else {
            return Err(mj(format!(
                "form `{name}` is on a page that is not rendered per request: forms need \
                 `mode = \"request\"` on their route (each visitor gets their own CSRF token)"
            )));
        };
        let path = request
            .get_attr("path")
            .ok()
            .and_then(|p| p.as_str().map(str::to_string))
            .unwrap_or_else(|| "/".into());
        let sent = request
            .get_attr("args")
            .ok()
            .and_then(|a| a.get_attr("sent").ok())
            .and_then(|s| s.as_str().map(str::to_string))
            .is_some_and(|s| s == name);
        // State from a post that is shown again (errors, next step).
        let st = lookup(state, "form_state").filter(|s| {
            s.get_attr("form")
                .ok()
                .and_then(|f| f.as_str().map(str::to_string))
                .as_deref()
                == Some(name.as_str())
        });
        let get = |k: &str| st.as_ref().and_then(|s| s.get_attr(k).ok());
        let values: BTreeMap<String, String> = get("values")
            .map(|v| {
                engine_core::templates::value_map(&v)
                    .into_iter()
                    .map(|(k, v)| (k, v.as_str().map(str::to_string).unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();
        let errors: BTreeMap<String, String> = get("errors")
            .map(|v| {
                engine_core::templates::value_map(&v)
                    .into_iter()
                    .map(|(k, v)| (k, v.as_str().map(str::to_string).unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();
        let step = get("step")
            .and_then(|v| v.as_usize())
            .unwrap_or(0)
            .min(def.step_count() - 1);
        let stamp = get("stamp")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| self.forms.stamp(&name));
        let shown: Vec<&def::Field> = def.step_fields(step);
        let fields: Vec<Value> = shown
            .iter()
            .map(|f| {
                let value = values.get(&f.name).cloned().unwrap_or_default();
                let options: Vec<Value> = f
                    .options
                    .iter()
                    .map(|o| minijinja::context! { value => o, selected => *o == value })
                    .collect();
                minijinja::context! {
                    name => f.name, id => format!("form-{name}-{}", f.name), label => f.label(),
                    type => f.kind.as_str(), required => f.required,
                    value => value, checked => !value.is_empty() && f.kind == def::Kind::Checkbox,
                    options => options, error => errors.get(&f.name),
                    help => f.help, placeholder => f.placeholder, autocomplete => f.autocomplete,
                    min_length => f.min_length, max_length => f.max_len(), min => f.min, max => f.max,
                    accept => (f.kind == def::Kind::File).then(|| f.accept_attr()),
                }
            })
            .collect();
        let shown_names: Vec<&str> = shown.iter().map(|f| f.name.as_str()).collect();
        let carried: Vec<Value> = values
            .iter()
            .filter(|(k, _)| !shown_names.contains(&k.as_str()) && def.field(k).is_some())
            .map(|(k, v)| minijinja::context! { name => k, value => v })
            .collect();
        let steps: Vec<Value> = def
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| minijinja::context! { title => s.title, number => i + 1, current => i == step, done => i < step })
            .collect();
        Ok(minijinja::context! {
            name => name, title => def.title, intro => def.intro, submit => def.submit,
            success => def.success, sent => sent,
            action => format!("/_engine/forms/{name}/"), page => path,
            csrf => csrf, stamp => stamp, honeypot => def.honeypot(),
            error => get("error"), errors => errors,
            fields => fields, carried => carried,
            step => step, steps => steps, step_count => def.step_count(),
            first_step => step == 0, last_step => step + 1 >= def.step_count(),
            error_count => errors.len(),
            multipart => def.fields.iter().any(|f| f.kind == def::Kind::File),
        })
    }

    /// `my_submissions(form=None, limit=50)`: the signed-in visitor's own
    /// submissions (empty for visitors and inside shared fragments).
    fn mine(&self, state: &State, kwargs: Kwargs) -> Result<Value, Error> {
        let form: Option<String> = kwargs.get("form")?;
        let limit: Option<i64> = kwargs.get("limit")?;
        kwargs.assert_all_used()?;
        let Some(id) = lookup(state, "user")
            .and_then(|u| u.get_attr("id").ok())
            .and_then(|v| i64::try_from(v).ok())
        else {
            return Ok(Value::from(Vec::<Value>::new()));
        };
        let Some(store) = self.forms.store() else {
            return Ok(Value::from(Vec::<Value>::new()));
        };
        // A new submission by this account drops fragments showing these.
        engine_core::tags::record(engine_core::tags::CacheTag::item(
            "submissions",
            id.to_string(),
        ));
        let subs = self
            .forms
            .block_on(store.submissions_of_user(
                id,
                form.as_deref(),
                limit.unwrap_or(50).clamp(1, 500),
            ))
            .map_err(engine_core::templates::fn_error)?;
        // Flatten for tables: id, form, created_at plus the fields.
        let rows: Vec<serde_json::Value> = subs
            .into_iter()
            .map(|s| {
                let mut m = serde_json::Map::new();
                m.insert("id".into(), s.id.into());
                m.insert("form".into(), s.form.into());
                m.insert("created_at".into(), s.created_at.into());
                if let serde_json::Value::Object(d) = s.data {
                    for (k, v) in d {
                        m.entry(k).or_insert(v);
                    }
                }
                serde_json::Value::Object(m)
            })
            .collect();
        // Rows depend on this user's submissions: never cache them shared.
        Ok(Value::from_serialize(&rows))
    }
}

impl SiteExtension for FormsExtension {
    fn extend_env(&self, env: &mut minijinja::Environment<'static>) {
        let me = Arc::new(FormsExtension {
            forms: self.forms.clone(),
        });
        let m = me.clone();
        env.add_function(
            "form_view",
            move |state: &State, name: String, kwargs: Kwargs| m.view(state, name, kwargs),
        );
        let m = me;
        env.add_function("my_submissions", move |state: &State, kwargs: Kwargs| {
            m.mine(state, kwargs)
        });
    }

    fn template_dirs(&self) -> Vec<&'static Dir<'static>> {
        component_dirs()
    }
}
