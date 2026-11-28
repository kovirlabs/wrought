//! `engine generate <kind> <name> [fields...]`: scaffolding for content
//! types, routes, forms, handlers, jobs and mailers, and `generate app`,
//! which makes a site its own Rust crate (an app crate, docs "App crates").
//!
//! Every generator works the same way: it plans the files it creates and
//! the files it edits (`site.toml`, the app's `lib.rs` or `main.rs`, a
//! module's `mod.rs`), refuses to replace a file that exists unless
//! `--force`, lists the plan and stops with `--dry-run`, then writes and
//! checks the result (the site's configuration, the type registry and its
//! migration). A failed check puts every file back as it was. The files
//! are made from the templates in `generators/`, embedded in the binary;
//! Rust files are formatted with `rustfmt` when it is on `PATH`.
//!
//! Handlers and jobs are Rust and live in an app crate. On a site that is
//! not one, they stop with a message pointing at `engine generate app`;
//! nothing is converted without being asked.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use engine_core::builder;
use engine_core::routes::{check_route_path, normalize_path};
use include_dir::{Dir, include_dir};

use crate::SiteArg;

/// The templates of generated files.
static GENERATORS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/generators");

#[derive(Args, Clone)]
pub struct Common {
    #[command(flatten)]
    site: SiteArg,
    /// Replace files that exist.
    #[arg(long)]
    force: bool,
    /// List what would be written; write nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand)]
pub enum GenerateCmd {
    /// A content type: types/<name>.toml, an item and a list template, their
    /// two `publish` routes and the type's migration (as `engine types
    /// sync` writes it). Fields: `name[:type[:arg]][!]`, `!` for required,
    /// e.g. `starts:date! kind:select:talk|workshop speaker:relation:person`.
    Type {
        /// Type name: lowercase letters, digits and `_` (`blog_post`).
        name: String,
        /// Fields (default: `title:text! body:markdown`; a `title` field is
        /// added when none is given).
        #[arg(value_name = "FIELD")]
        fields: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// A page: a [[routes]] entry in site.toml and its template.
    Route {
        /// The page's path, e.g. `/about/team/`.
        path: String,
        /// Template (default: made from the path, `about_team.html`); an
        /// existing one is used as it is.
        #[arg(long)]
        template: Option<String>,
        /// build, publish or request (default: the site's `[render]
        /// default`).
        #[arg(long)]
        mode: Option<String>,
        /// The page title (default: from the path).
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// A form: forms/<name>.toml, its page (a `request` route with the form
    /// component). Fields: `name[:type[:options]][!]`, e.g. `email:email!
    /// topic:select:Billing|Bug`.
    Form {
        name: String,
        /// Fields (default: `name! email:email! message:textarea!`).
        #[arg(value_name = "FIELD")]
        fields: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// An HTTP handler in the site's app crate: src/handlers/<name>.rs,
    /// registered with `App::route`, a test, and for GET a template.
    Handler {
        name: String,
        /// Path (default: `/<name>/`); axum syntax for parameters
        /// (`/notes/{id}/`).
        #[arg(long)]
        path: Option<String>,
        /// get (renders a template), or post, put, patch, delete (take
        /// JSON).
        #[arg(long, default_value = "get")]
        method: String,
        #[command(flatten)]
        common: Common,
    },
    /// A job handler in the site's app crate: src/jobs/<name>.rs with its
    /// payload and a test, registered with `App::job`. Payload fields:
    /// `name:type` with i64, f64, bool or string.
    Job {
        /// The job kind, e.g. `report.daily` (`[a-z][a-z0-9_.-]*`).
        kind: String,
        /// Payload fields (default: `id:i64`).
        #[arg(value_name = "FIELD")]
        fields: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// An email: templates/mail/<name>.txt and .html and, in an app crate,
    /// a sender (src/mailers/<name>.rs) with a test. Fields: `name:type`
    /// with i64, f64, bool or string.
    Mailer {
        name: String,
        #[arg(value_name = "FIELD")]
        fields: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Make the site its own Rust crate (an app crate): Cargo.toml,
    /// src/main.rs, src/lib.rs and a test, next to site.toml.
    App {
        /// The crate's name (default: the site folder's name).
        #[arg(long)]
        name: Option<String>,
        /// Depend on the engine at this path (a checkout's
        /// crates/engine-cli) instead of its crates.io version.
        #[arg(long)]
        engine_path: Option<PathBuf>,
        /// The engine features the site uses (a preset or single
        /// features, comma-separated).
        #[arg(long, default_value = "app", value_delimiter = ',')]
        features: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
}

pub fn run(cmd: GenerateCmd) -> Result<()> {
    match cmd {
        GenerateCmd::Type {
            name,
            fields,
            common,
        } => apply(type_plan(&common.site.site, &name, &fields)?, &common),
        GenerateCmd::Route {
            path,
            template,
            mode,
            title,
            common,
        } => apply(
            route_plan(
                &common.site.site,
                &path,
                template,
                mode.as_deref(),
                title,
                common.force,
            )?,
            &common,
        ),
        GenerateCmd::Form {
            name,
            fields,
            common,
        } => apply(form_plan(&common.site.site, &name, &fields)?, &common),
        GenerateCmd::Handler {
            name,
            path,
            method,
            common,
        } => apply(
            handler_plan(&common.site.site, &name, path.as_deref(), &method)?,
            &common,
        ),
        GenerateCmd::Job {
            kind,
            fields,
            common,
        } => apply(job_plan(&common.site.site, &kind, &fields)?, &common),
        GenerateCmd::Mailer {
            name,
            fields,
            common,
        } => apply(mailer_plan(&common.site.site, &name, &fields)?, &common),
        GenerateCmd::App {
            name,
            engine_path,
            features,
            common,
        } => apply(
            app_plan(
                &common.site.site,
                name.as_deref(),
                engine_path.as_deref(),
                &features,
            )?,
            &common,
        ),
    }
}

// ----- plans ---------------------------------------------------------------

/// A file to write (relative to the site folder).
struct Change {
    path: PathBuf,
    text: String,
    /// An edit of a file that may exist (`site.toml`, `mod.rs`...), not a
    /// file of the generator's own: never refused.
    edit: bool,
}

/// A check after writing; returns notes to print.
type Check = Box<dyn FnOnce(&Path) -> Result<Vec<String>>>;

/// What a generator does.
#[derive(Default)]
struct Plan {
    changes: Vec<Change>,
    /// Written Rust files, for rustfmt.
    rust: Vec<PathBuf>,
    /// Checks after writing (an error puts everything back).
    check: Option<Check>,
    /// What to do next.
    next: Vec<String>,
}

impl Plan {
    fn create(&mut self, path: impl Into<PathBuf>, text: String) {
        let path = path.into();
        if path.extension().is_some_and(|e| e == "rs") {
            self.rust.push(path.clone());
        }
        self.changes.push(Change {
            path,
            text,
            edit: false,
        });
    }

    fn edit(&mut self, path: impl Into<PathBuf>, text: String) {
        let path = path.into();
        if path.extension().is_some_and(|e| e == "rs") {
            self.rust.push(path.clone());
        }
        self.changes.push(Change {
            path,
            text,
            edit: true,
        });
    }
}

/// Write a plan (or list it), check it, and put things back on failure.
fn apply(plan: Plan, common: &Common) -> Result<()> {
    let site = &common.site.site;
    let mut refused = Vec::new();
    for c in &plan.changes {
        if !c.edit && site.join(&c.path).exists() && !common.force {
            refused.push(c.path.display().to_string());
        }
    }
    if !refused.is_empty() {
        bail!(
            "{} exist{}; nothing was written (--force replaces {})",
            refused.join(", "),
            if refused.len() == 1 { "s" } else { "" },
            if refused.len() == 1 { "it" } else { "them" }
        );
    }
    let verb = |c: &Change| match (site.join(&c.path).exists(), c.edit) {
        (false, _) => "create",
        (true, true) => "edit",
        (true, false) => "replace",
    };
    if common.dry_run {
        for c in &plan.changes {
            println!("would {:<7} {}", verb(c), c.path.display());
        }
        if plan.check.is_some() {
            println!("then check the site (and, for a type, write its migration)");
        }
        return Ok(());
    }
    let mut undo = Undo::default();
    let result = (|| -> Result<Vec<String>> {
        for c in &plan.changes {
            let full = site.join(&c.path);
            println!("{:<7} {}", verb(c), c.path.display());
            undo.write(&full, &c.text)?;
        }
        let mut notes = rustfmt(site, &plan.rust);
        if let Some(check) = plan.check {
            notes.extend(check(site)?);
        }
        Ok(notes)
    })();
    match result {
        Ok(notes) => {
            for n in notes {
                println!("{n}");
            }
            if !plan.next.is_empty() {
                println!("Next:");
                for n in &plan.next {
                    println!("  {n}");
                }
            }
            Ok(())
        }
        Err(e) => {
            undo.restore();
            Err(e.context("nothing was changed"))
        }
    }
}

/// What a generator wrote, to put back.
#[derive(Default)]
struct Undo {
    files: Vec<(PathBuf, Option<Vec<u8>>)>,
    dirs: Vec<PathBuf>,
}

impl Undo {
    fn write(&mut self, path: &Path, text: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            let mut missing = Vec::new();
            let mut d = parent;
            while !d.as_os_str().is_empty() && !d.exists() {
                missing.push(d.to_path_buf());
                match d.parent() {
                    Some(p) => d = p,
                    None => break,
                }
            }
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
            self.dirs.extend(missing);
        }
        if !self.files.iter().any(|(p, _)| p == path) {
            self.files
                .push((path.to_path_buf(), std::fs::read(path).ok()));
        }
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// Put every file back (and drop the folders made for new ones).
    fn restore(self) {
        for (p, old) in self.files.into_iter().rev() {
            let _ = match old {
                Some(b) => std::fs::write(&p, b),
                None => std::fs::remove_file(&p),
            };
        }
        for d in self.dirs {
            let _ = std::fs::remove_dir(&d);
        }
    }
}

/// Format the written Rust files with rustfmt, when there is one.
fn rustfmt(site: &Path, files: &[PathBuf]) -> Vec<String> {
    if files.is_empty() {
        return Vec::new();
    }
    let mut c = std::process::Command::new("rustfmt");
    c.args(["--edition", "2024"]).args(files).current_dir(site);
    match c.output() {
        Ok(o) if o.status.success() => Vec::new(),
        Ok(o) => vec![format!(
            "note: rustfmt did not format the Rust files: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )],
        Err(_) => vec!["note: rustfmt is not on PATH; the Rust files are not formatted".into()],
    }
}

// ----- names -----------------------------------------------------------------

/// A name for a type, form, handler or mail: lowercase letters, digits and
/// `_` (a `-` becomes `_`), starting with a letter.
fn ident(what: &str, name: &str) -> Result<String> {
    let n = name.trim().replace('-', "_");
    let ok = n.len() <= 40
        && n.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        bail!(
            "{what} `{name}`: names are lowercase letters, digits and `_`, starting with a \
             letter (at most 40), e.g. `blog_post`"
        );
    }
    Ok(n)
}

/// `order_shipped` as `Order shipped`.
fn label(name: &str) -> String {
    let words = name.replace(['_', '-', '.'], " ");
    let mut c = words.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// `order_shipped` as `OrderShipped`.
fn camel(name: &str) -> String {
    name.split(['_', '-', '.'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect()
}

/// English plural of a lowercase word: `event` `events`, `category`
/// `categories`, `box` `boxes`.
fn plural(word: &str) -> String {
    if let Some(stem) = word.strip_suffix('y')
        && !stem.ends_with(['a', 'e', 'i', 'o', 'u'])
    {
        return format!("{stem}ies");
    }
    if word.ends_with('s')
        || word.ends_with('x')
        || word.ends_with('z')
        || word.ends_with("ch")
        || word.ends_with("sh")
    {
        return format!("{word}es");
    }
    format!("{word}s")
}

/// A name as a URL segment (`blog_post` as `blog-post`).
fn segment(name: &str) -> String {
    name.replace('_', "-")
}

/// A string as a TOML string (quoted, escaped).
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// A string as a Rust string literal.
fn rust_str(s: &str) -> String {
    format!("{s:?}")
}

/// The layout generated pages extend: the site's `layout.html` when it has
/// one (the app preset's), else `base.html` (the site's or the built-in).
fn layout(site: &Path) -> &'static str {
    if site.join("templates/layout.html").is_file() {
        "layout.html"
    } else {
        "base.html"
    }
}

/// A generator template with its tokens replaced.
fn fill(file: &str, vars: &[(&str, &str)]) -> String {
    let mut s = GENERATORS
        .get_file(file)
        .and_then(|f| f.contents_utf8())
        .unwrap_or_else(|| panic!("generators/{file} is embedded"))
        .to_string();
    for (k, v) in vars {
        s = s.replace(&format!("__{k}__"), v);
    }
    s
}

// ----- fields ----------------------------------------------------------------

/// `name[:kind[:arg]][!]`.
#[derive(Debug, Clone, PartialEq)]
struct Field {
    name: String,
    kind: String,
    arg: Option<String>,
    required: bool,
}

fn parse_fields(specs: &[String], default_kind: &str) -> Result<Vec<Field>> {
    let mut out: Vec<Field> = Vec::new();
    for spec in specs {
        let (body, required) = match spec.strip_suffix('!') {
            Some(b) => (b, true),
            None => (spec.as_str(), false),
        };
        let mut parts = body.splitn(3, ':');
        let name = ident("field", parts.next().unwrap_or_default())?;
        let kind = parts
            .next()
            .filter(|k| !k.is_empty())
            .unwrap_or(default_kind)
            .to_string();
        let arg = parts.next().map(str::to_string);
        if out.iter().any(|f| f.name == name) {
            bail!("field `{name}` is given twice");
        }
        out.push(Field {
            name,
            kind,
            arg,
            required,
        });
    }
    Ok(out)
}

/// `a|b|c` as a TOML array of strings.
fn toml_list(arg: &str) -> String {
    let items: Vec<String> = arg
        .split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(toml_str)
        .collect();
    format!("[{}]", items.join(", "))
}

/// A Rust payload or data field: (Rust type, sample JSON, sample Rust).
fn rust_field(f: &Field) -> Result<(&'static str, &'static str, &'static str)> {
    Ok(match f.kind.as_str() {
        "i64" | "int" | "integer" => ("i64", "1", "1"),
        "f64" | "float" | "number" => ("f64", "1.5", "1.5"),
        "bool" | "boolean" => ("bool", "true", "true"),
        "string" | "str" | "text" | "String" => ("String", "\"x\"", "\"x\".into()"),
        other => bail!(
            "field `{}`: type `{other}` is not one of i64, f64, bool, string",
            f.name
        ),
    })
}

/// The pieces of a Rust struct with `fields`: (declaration body, names for
/// a pattern, sample JSON object body, sample struct literal body).
fn rust_struct(fields: &[Field]) -> Result<(String, String, String, String)> {
    if fields.is_empty() {
        return Ok(Default::default());
    }
    let mut decl = Vec::new();
    let mut names = Vec::new();
    let mut json = Vec::new();
    let mut lit = Vec::new();
    for f in fields {
        let (ty, j, r) = rust_field(f)?;
        decl.push(format!("pub {}: {ty},", f.name));
        names.push(f.name.clone());
        json.push(format!("{:?}: {j}", f.name));
        lit.push(format!("{}: {r}", f.name));
    }
    Ok((
        format!(" {} ", decl.join(" ")),
        format!(" {} ", names.join(", ")),
        format!(" {} ", json.join(", ")),
        format!(" {} ", lit.join(", ")),
    ))
}

// ----- site.toml ---------------------------------------------------------------

/// `site.toml`'s text and its routes' paths.
fn site_toml(site: &Path) -> Result<(String, Vec<String>)> {
    let (cfg, _) = builder::load_config(site)?;
    let text = std::fs::read_to_string(site.join("site.toml"))?;
    Ok((
        text,
        cfg.routes.iter().map(|r| normalize_path(&r.path)).collect(),
    ))
}

/// `site.toml` with `blocks` appended (each a `[[routes]]` table).
fn append_routes(text: &str, blocks: &[String]) -> String {
    let mut s = text.trim_end().to_string();
    s.push('\n');
    for b in blocks {
        s.push('\n');
        s.push_str(b);
    }
    s
}

/// One `[[routes]]` table.
fn route_block(comment: &str, pairs: &[(&str, String)]) -> String {
    let mut s = format!("# {comment}\n[[routes]]\n");
    for (k, v) in pairs {
        s.push_str(&format!("{k} = {v}\n"));
    }
    s
}

fn check_config(site: &Path) -> Result<Vec<String>> {
    builder::load_config(site)?;
    Ok(Vec::new())
}

// ----- type --------------------------------------------------------------------

const TYPE_KINDS: &[&str] = &[
    "text", "markdown", "integer", "boolean", "date", "url", "select", "relation", "terms",
    "blocks", "media", "token",
];

fn type_plan(site: &Path, name: &str, specs: &[String]) -> Result<Plan> {
    let name = ident("type", name)?;
    let (toml_text, paths) = site_toml(site)?;
    let mut fields = parse_fields(specs, "text")?;
    if fields.is_empty() {
        fields = parse_fields(&["title!".into(), "body:markdown".into()], "text")?;
    }
    let mut notes = Vec::new();
    if !fields.iter().any(|f| f.name == "title") {
        fields.insert(
            0,
            Field {
                name: "title".into(),
                kind: "text".into(),
                arg: None,
                required: true,
            },
        );
        notes.push("note: added a `title` field (the pages and the list show it)".to_string());
    }
    let label_ = label(&name);
    let plural_name = plural(&name);
    let list_path = format!("/{}/", segment(&plural_name));
    let item_path = format!("{list_path}{{slug}}/");
    let list_template = if plural_name == name {
        format!("{name}_list.html")
    } else {
        format!("{plural_name}.html")
    };
    let item_template = format!("{name}.html");
    for p in [&list_path, &item_path] {
        if paths.contains(&normalize_path(p)) {
            bail!("site.toml already has a route at {p}");
        }
    }

    // types/<name>.toml
    let mut t = format!(
        "# {label_} (generated by `engine generate type`): one page per published item\n\
         # at {item_path}, listed at {list_path}. Fields: see \"Content types\" in the docs.\n\
         label = {}\n",
        toml_str(&label_)
    );
    let mut item_fields = String::new();
    for f in &fields {
        if !TYPE_KINDS.contains(&f.kind.as_str()) {
            bail!(
                "field `{}`: `{}` is not a field type ({})",
                f.name,
                f.kind,
                TYPE_KINDS.join(", ")
            );
        }
        t.push_str(&format!(
            "\n[[fields]]\nname = {}\ntype = {}\n",
            toml_str(&f.name),
            toml_str(&f.kind)
        ));
        if f.required {
            t.push_str("required = true\n");
        }
        let need = |what: &str| -> Result<&str> {
            f.arg.as_deref().filter(|a| !a.is_empty()).ok_or_else(|| {
                anyhow::anyhow!(
                    "field `{}`: a {} field needs {what} (`{}:{}:...`)",
                    f.name,
                    f.kind,
                    f.name,
                    f.kind
                )
            })
        };
        match f.kind.as_str() {
            "text" if f.name == "title" => t.push_str("max_length = 120\n"),
            "select" => t.push_str(&format!("options = {}\n", toml_list(need("options")?))),
            "relation" => t.push_str(&format!("to = {}\n", toml_str(need("a type")?))),
            "terms" => t.push_str(&format!(
                "vocabulary = {}\n",
                toml_str(need("a vocabulary")?)
            )),
            "blocks" => t.push_str(&format!("blocks = {}\n", toml_list(need("block types")?))),
            "token" => t.push_str(&format!("token = {}\n", toml_str(need("a token group")?))),
            _ if f.arg.is_some() => bail!(
                "field `{}`: a {} field takes no `:{}`",
                f.name,
                f.kind,
                f.arg.as_deref().unwrap_or_default()
            ),
            _ => {}
        }
        if f.name != "title" {
            item_fields.push_str(&item_field(f));
        }
    }

    let layout = layout(site);
    let plural_label = plural_name.replace('_', " ");
    let vars = [
        ("name", name.as_str()),
        ("layout", layout),
        ("item_path", item_path.as_str()),
        ("list_path", list_path.as_str()),
        ("plural_label", plural_label.as_str()),
        ("item_fields", item_fields.as_str()),
    ];
    let mut plan = Plan::default();
    plan.create(format!("types/{name}.toml"), t);
    plan.create(
        format!("templates/{item_template}"),
        fill("type/item.html", &vars),
    );
    plan.create(
        format!("templates/{list_template}"),
        fill("type/list.html", &vars),
    );
    plan.edit(
        "site.toml",
        append_routes(
            &toml_text,
            &[
                route_block(
                    &format!("The published `{name}` items (engine generate type)."),
                    &[
                        ("path", toml_str(&list_path)),
                        ("template", toml_str(&list_template)),
                        ("mode", toml_str("publish")),
                        ("title", toml_str(&label(&plural_name))),
                    ],
                ),
                route_block(
                    &format!("One page per published `{name}` (engine generate type)."),
                    &[
                        ("path", toml_str(&item_path)),
                        ("type", toml_str(&name)),
                        ("template", toml_str(&item_template)),
                        ("mode", toml_str("publish")),
                    ],
                ),
            ],
        ),
    );
    plan.check = Some(Box::new(move |site: &Path| {
        check_config(site)?;
        notes.extend(type_migration(site)?);
        Ok(notes)
    }));
    plan.next = vec![
        "engine migrate".into(),
        format!(
            "add {name} items in the admin, or: engine content create {name} --set title=... --publish"
        ),
        "engine build".into(),
    ];
    Ok(plan)
}

/// A field on the item page.
fn item_field(f: &Field) -> String {
    let n = &f.name;
    let l = label(n);
    match f.kind.as_str() {
        "text" => format!("{{% if page.{n} %}}<p>{{{{ page.{n} }}}}</p>{{% endif %}}\n"),
        "markdown" => format!("{{% if page.{n} %}}{{{{ page.{n} }}}}{{% endif %}}\n"),
        "boolean" => format!("<p>{l}: {{{{ \"yes\" if page.{n} else \"no\" }}}}</p>\n"),
        "date" => format!(
            "{{% if page.{n} %}}<p>{l}: <time datetime=\"{{{{ page.{n} }}}}\">{{{{ page.{n} | date }}}}</time></p>{{% endif %}}\n"
        ),
        "url" => format!(
            "{{% if page.{n} %}}<p><a href=\"{{{{ page.{n} }}}}\">{l}</a></p>{{% endif %}}\n"
        ),
        "blocks" => {
            format!("{{% for b in page.{n} or [] %}}{{{{ render_block(b) }}}}{{% endfor %}}\n")
        }
        "integer" | "select" | "token" => format!(
            "{{% if page.{n} is defined and page.{n} is not none %}}<p>{l}: {{{{ page.{n} }}}}</p>{{% endif %}}\n"
        ),
        kind => format!(
            "{{#- page.{n}: a {kind} field; see \"Content types\" in the docs for what it holds. -#}}\n"
        ),
    }
}

/// The type's migration, as `engine types sync` writes it.
#[cfg(feature = "content")]
fn type_migration(site: &Path) -> Result<Vec<String>> {
    let reg = engine_content::Registry::load(site)?;
    let out = engine_content::schema::sync(
        site,
        &reg,
        engine_content::schema::SyncOptions {
            check: false,
            allow_drop: false,
            snapshot_only: false,
        },
    )?;
    let mut notes = Vec::new();
    for f in &out.files {
        let rel = f.strip_prefix(site).unwrap_or(f);
        println!("{:<7} {}", "create", rel.display());
    }
    if out.plan.summary.len() > 1 {
        notes.push(format!(
            "note: the migration also has the other unsynced type changes: {}",
            out.plan.summary.join("; ")
        ));
    }
    Ok(notes)
}

#[cfg(not(feature = "content"))]
fn type_migration(_site: &Path) -> Result<Vec<String>> {
    Ok(vec![
        "note: this build has no `content` feature: write the migration with `engine types \
         sync` in a cms or app build"
            .into(),
    ])
}

// ----- route -------------------------------------------------------------------

fn route_plan(
    site: &Path,
    path: &str,
    template: Option<String>,
    mode: Option<&str>,
    title: Option<String>,
    force: bool,
) -> Result<Plan> {
    if !path.starts_with('/') {
        bail!("route `{path}`: paths start with `/`");
    }
    if path.contains(['{', '*']) {
        bail!(
            "route `{path}`: a page per item is a type's route (`engine generate type`); \
             parameters and wildcards are written by hand"
        );
    }
    let path = normalize_path(path);
    if !path.ends_with('/') {
        bail!("route `{path}`: `engine generate route` makes HTML pages (paths ending in `/`)");
    }
    check_route_path(&path).with_context(|| format!("route `{path}`"))?;
    if crate::app::reserved_prefix(&path).is_some() {
        bail!("route `{path}`: that path belongs to the engine");
    }
    if let Some(m) = mode
        && !["build", "publish", "request"].contains(&m)
    {
        bail!("--mode `{m}`: build, publish or request");
    }
    let (toml_text, paths) = site_toml(site)?;
    if paths.contains(&path) {
        bail!("site.toml already has a route at {path}");
    }
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let stem = if segs.is_empty() {
        "home".to_string()
    } else {
        segs.join("_").replace(['-', '.', '~'], "_")
    };
    let mut plan = Plan::default();
    let given = template.is_some();
    let template = template.unwrap_or_else(|| format!("{stem}.html"));
    engine_core::templates::validate_template_name(&template)
        .map_err(|e| anyhow::anyhow!("--template: {e}"))?;
    let exists = site.join("templates").join(&template).is_file();
    if !(given && exists && !force) {
        plan.create(
            format!("templates/{template}"),
            fill(
                "route/page.html",
                &[
                    ("path", path.as_str()),
                    ("layout", layout(site)),
                    ("template", template.as_str()),
                ],
            ),
        );
    }
    let title = title.unwrap_or_else(|| {
        segs.last()
            .map(|s| label(s))
            .unwrap_or_else(|| "Home".into())
    });
    let mut pairs = vec![("path", toml_str(&path)), ("template", toml_str(&template))];
    if let Some(m) = mode {
        pairs.push(("mode", toml_str(m)));
    }
    pairs.push(("title", toml_str(&title)));
    plan.edit(
        "site.toml",
        append_routes(
            &toml_text,
            &[route_block("A page (engine generate route).", &pairs)],
        ),
    );
    plan.check = Some(Box::new(check_config));
    plan.next = vec!["engine dev".into()];
    Ok(plan)
}

// ----- form --------------------------------------------------------------------

const FORM_KINDS: &[&str] = &[
    "text", "email", "textarea", "number", "select", "radio", "checkbox", "date", "url", "tel",
    "file",
];

fn form_plan(site: &Path, name: &str, specs: &[String]) -> Result<Plan> {
    let name = ident("form", name)?;
    let (toml_text, paths) = site_toml(site)?;
    let mut fields = parse_fields(specs, "text")?;
    if fields.is_empty() {
        fields = parse_fields(
            &[
                "name!".into(),
                "email:email!".into(),
                "message:textarea!".into(),
            ],
            "text",
        )?;
    }
    let label_ = label(&name);
    let page = format!("/{}/", segment(&name));
    if paths.contains(&page) {
        bail!("site.toml already has a route at {page}");
    }
    let mut t = format!(
        "# {label_} (generated by `engine generate form`): shown at {page} by\n\
         # templates/{name}.html with component(\"form\", name=\"{name}\"), posted to\n\
         # /_engine/forms/{name}/ and stored; see \"Forms\" in the docs for\n\
         # [receipt], [notify] and the other settings.\n\
         title = {}\n\
         submit = \"Send\"\n\
         success = \"Thank you. We have received it.\"\n\
         \n\
         [spam]\n\
         min_seconds = 2\n\
         max_per_hour = 20\n",
        toml_str(&label_)
    );
    for f in &fields {
        if !FORM_KINDS.contains(&f.kind.as_str()) {
            bail!(
                "field `{}`: `{}` is not a form field type ({})",
                f.name,
                f.kind,
                FORM_KINDS.join(", ")
            );
        }
        t.push_str(&format!(
            "\n[[fields]]\nname = {}\nlabel = {}\n",
            toml_str(&f.name),
            toml_str(&label(&f.name))
        ));
        if f.kind != "text" {
            t.push_str(&format!("type = {}\n", toml_str(&f.kind)));
        }
        if f.required {
            t.push_str("required = true\n");
        }
        match (f.kind.as_str(), f.arg.as_deref()) {
            ("select" | "radio", Some(a)) if !a.is_empty() => {
                t.push_str(&format!("options = {}\n", toml_list(a)))
            }
            ("select" | "radio", _) => bail!(
                "field `{}`: a {} field needs options (`{}:{}:One|Two`)",
                f.name,
                f.kind,
                f.name,
                f.kind
            ),
            (_, Some(a)) => bail!("field `{}`: a {} field takes no `:{a}`", f.name, f.kind),
            _ => {}
        }
        match (f.name.as_str(), f.kind.as_str()) {
            (_, "email") => t.push_str("autocomplete = \"email\"\n"),
            ("name", "text") => t.push_str("autocomplete = \"name\"\n"),
            (_, "textarea") => t.push_str("max_length = 4000\n"),
            (_, "text") => t.push_str("max_length = 200\n"),
            _ => {}
        }
    }
    #[cfg(feature = "forms")]
    engine_forms::FormDef::parse_toml(&name, &t)
        .with_context(|| format!("the generated forms/{name}.toml"))?;

    let mut plan = Plan::default();
    plan.create(format!("forms/{name}.toml"), t);
    plan.create(
        format!("templates/{name}.html"),
        fill(
            "form/page.html",
            &[("name", name.as_str()), ("layout", layout(site))],
        ),
    );
    plan.edit(
        "site.toml",
        append_routes(
            &toml_text,
            &[route_block(
                &format!(
                    "The `{name}` form (engine generate form), per request: a form token \
                     each."
                ),
                &[
                    ("path", toml_str(&page)),
                    ("template", toml_str(&format!("{name}.html"))),
                    ("mode", toml_str("request")),
                    ("title", toml_str(&label_)),
                ],
            )],
        ),
    );
    plan.check = Some(Box::new(check_config));
    plan.next = vec![
        "engine migrate   # the forms tables, once".into(),
        format!("engine serve     # then open {page}"),
    ];
    Ok(plan)
}

// ----- app crates --------------------------------------------------------------

/// A site that is its own Rust crate.
struct AppCrate {
    /// The binary's name (`CARGO_BIN_EXE_<bin>` in tests).
    bin: String,
    /// The file with `App::new()`: `src/lib.rs` or `src/main.rs`.
    reg_file: PathBuf,
    /// Whether that is a library (modules are `pub mod`).
    lib: bool,
    deps: toml::Table,
}

impl AppCrate {
    fn has_dep(&self, name: &str) -> bool {
        self.deps.contains_key(name)
    }

    fn need_dep(&self, name: &str, line: &str, what: &str) -> Result<()> {
        if !self.has_dep(name) {
            bail!("{what} needs `{name}` in Cargo.toml's [dependencies]: add\n  {line}");
        }
        Ok(())
    }
}

/// Whether the site folder is an app crate (a Cargo.toml depending on
/// engine-cli next to site.toml).
pub(crate) fn is_app_crate(site: &Path) -> bool {
    cargo_toml(site).is_some_and(|t| deps(&t).contains_key("engine-cli"))
}

fn cargo_toml(site: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(site.join("Cargo.toml"))
        .ok()?
        .parse::<toml::Table>()
        .ok()
}

fn deps(t: &toml::Table) -> toml::Table {
    t.get("dependencies")
        .and_then(|d| d.as_table())
        .cloned()
        .unwrap_or_default()
}

fn app_crate(site: &Path, what: &str) -> Result<AppCrate> {
    let not_one = || {
        anyhow::anyhow!(
            "{what} lives in the site's own Rust crate, and {} is not one (no Cargo.toml \
             depending on engine-cli next to site.toml). Make it one with `engine generate \
             app --site {}` (docs: \"App crates\"), then run this again.",
            site.display(),
            site.display()
        )
    };
    let t = cargo_toml(site).ok_or_else(not_one)?;
    let deps = deps(&t);
    if !deps.contains_key("engine-cli") {
        return Err(not_one());
    }
    let package = t
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .context("Cargo.toml has no [package] name")?;
    let bin = t
        .get("bin")
        .and_then(|b| b.as_array())
        .and_then(|b| b.first())
        .and_then(|b| b.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or(package)
        .to_string();
    for (file, lib) in [("src/lib.rs", true), ("src/main.rs", false)] {
        if std::fs::read_to_string(site.join(file)).is_ok_and(|s| find_app_new(&s).is_some()) {
            return Ok(AppCrate {
                bin,
                reg_file: PathBuf::from(file),
                lib,
                deps,
            });
        }
    }
    bail!(
        "found no `App::new()` in src/lib.rs or src/main.rs to register the {what} on \
         (docs: \"App crates\")"
    )
}

/// The line with `App::new()` (not in a comment).
fn find_app_new(src: &str) -> Option<usize> {
    src.lines()
        .position(|l| !l.trim_start().starts_with("//") && l.contains("App::new()"))
}

/// `src` with `line` (`.route(...)`) chained right after `App::new()`.
fn register(src: &str, line: &str) -> Result<String> {
    let i = find_app_new(src).context("no `App::new()` to register on")?;
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();
    let l = lines[i].clone();
    let at = l.find("App::new()").expect("found above") + "App::new()".len();
    let (head, rest) = l.split_at(at);
    let indent: String = l.chars().take_while(|c| c.is_whitespace()).collect();
    let inner = format!("{indent}    ");
    let mut new = vec![head.to_string(), format!("{inner}{line}")];
    if !rest.trim().is_empty() {
        new.push(format!("{inner}{}", rest.trim_start()));
    }
    lines.splice(i..=i, new);
    Ok(lines.join("\n") + "\n")
}

/// `src` with the module declaration `decl` (`pub mod jobs;`), unless it
/// has it: after the last `mod`, else after the `use` lines, else after
/// the file's doc comment.
fn declare_mod(src: &str, decl: &str) -> String {
    let bare = decl.trim_start_matches("pub ");
    let lines: Vec<&str> = src.lines().collect();
    if lines
        .iter()
        .any(|l| l.trim() == bare || l.trim() == format!("pub {bare}"))
    {
        return src.to_string();
    }
    let top = |l: &&str| !l.starts_with(char::is_whitespace);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    if let Some(i) = lines
        .iter()
        .rposition(|l| top(l) && (l.starts_with("mod ") || l.starts_with("pub mod ")))
    {
        out.insert(i + 1, decl.to_string());
    } else if let Some(u) = lines
        .iter()
        .rposition(|l| top(l) && (l.starts_with("use ") || l.starts_with("pub use ")))
    {
        // The end of that `use` (it may span lines).
        let end = (u..lines.len())
            .find(|j| lines[*j].trim_end().ends_with(';'))
            .unwrap_or(u);
        out.insert(end + 1, String::new());
        out.insert(end + 2, decl.to_string());
    } else {
        let start = lines
            .iter()
            .position(|l| !(l.starts_with("//!") || l.trim().is_empty()))
            .unwrap_or(lines.len());
        out.insert(start, decl.to_string());
        if start < lines.len() {
            out.insert(start + 1, String::new());
        }
    }
    out.join("\n") + "\n"
}

/// The edits that add module `module` under `src/<dir>/`: its `mod.rs`
/// and the declaration of `<dir>` in the registration file.
fn add_module(
    plan: &mut Plan,
    site: &Path,
    app: &AppCrate,
    reg_src: &mut String,
    dir: &str,
    module: &str,
    doc: &str,
) -> Result<()> {
    let mod_rs = format!("src/{dir}/mod.rs");
    let existing =
        std::fs::read_to_string(site.join(&mod_rs)).unwrap_or_else(|_| format!("//! {doc}\n\n"));
    plan.edit(
        &mod_rs,
        declare_mod(&existing, &format!("pub mod {module};")),
    );
    let vis = if app.lib { "pub mod" } else { "mod" };
    *reg_src = declare_mod(reg_src, &format!("{vis} {dir};"));
    Ok(())
}

fn handler_plan(site: &Path, name: &str, path: Option<&str>, method: &str) -> Result<Plan> {
    let name = ident("handler", name)?;
    let app = app_crate(site, "a handler")?;
    app.need_dep(
        "axum",
        "axum = { version = \"0.8\", default-features = false, features = [\"form\", \"json\"] }",
        "a handler",
    )?;
    let method = method.to_ascii_lowercase();
    if !["get", "post", "put", "patch", "delete"].contains(&method.as_str()) {
        bail!("--method `{method}`: get, post, put, patch or delete");
    }
    let path = path.map_or_else(|| format!("/{}/", segment(&name)), str::to_string);
    if !path.starts_with('/') {
        bail!("--path `{path}`: paths start with `/`");
    }
    if let Some(r) = crate::app::reserved_prefix(&path) {
        bail!("--path `{path}`: paths under `{r}` belong to the engine");
    }
    let mut reg_src = std::fs::read_to_string(site.join(&app.reg_file))?;
    if reg_src.contains(&format!(".route({}", rust_str(&path))) {
        bail!("{} already registers {path}", app.reg_file.display());
    }
    let template = format!("{name}.html");
    let up = method.to_ascii_uppercase();
    let label_ = label(&name);
    let vars = [
        ("name", name.as_str()),
        ("module", name.as_str()),
        ("path", path.as_str()),
        ("template", template.as_str()),
        ("Label", label_.as_str()),
        ("METHOD", up.as_str()),
        ("layout", layout(site)),
        ("bin", app.bin.as_str()),
    ];
    let mut plan = Plan::default();
    if method == "get" {
        plan.create(
            format!("src/handlers/{name}.rs"),
            fill("handler/get.rs", &vars),
        );
        plan.create(
            format!("templates/{template}"),
            fill("handler/page.html", &vars),
        );
    } else {
        plan.create(
            format!("src/handlers/{name}.rs"),
            fill("handler/json.rs", &vars),
        );
    }
    plan.create(
        format!("tests/handler_{name}.rs"),
        fill("handler/test.rs", &vars),
    );
    add_module(
        &mut plan,
        site,
        &app,
        &mut reg_src,
        "handlers",
        &name,
        "The site's HTTP handlers, registered in the app with `App::route`.",
    )?;
    reg_src = register(
        &reg_src,
        &format!(
            ".route({}, axum::routing::{method}(handlers::{name}::handle))",
            rust_str(&path)
        ),
    )?;
    plan.edit(app.reg_file.clone(), reg_src);
    plan.next = vec![
        "cargo test".into(),
        format!("cargo run -- serve   # then {up} {path}"),
    ];
    Ok(plan)
}

/// Job kinds of the engine (`App::job` refuses them).
const RESERVED_JOBS: &[&str] = &["engine.", "mail.", "payments."];

fn job_plan(site: &Path, kind: &str, specs: &[String]) -> Result<Plan> {
    let valid = kind.len() <= 64
        && kind.starts_with(|c: char| c.is_ascii_lowercase())
        && kind
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
    if !valid {
        bail!("job `{kind}`: kinds are `[a-z][a-z0-9_.-]*`, at most 64 characters");
    }
    if let Some(r) = RESERVED_JOBS.iter().find(|r| kind.starts_with(*r)) {
        bail!("job `{kind}`: the `{r}` prefix belongs to the engine");
    }
    let module = kind.replace(['.', '-'], "_");
    let app = app_crate(site, "a job")?;
    app.need_dep(
        "serde",
        "serde = { version = \"1\", features = [\"derive\"] }",
        "a job",
    )?;
    let mut reg_src = std::fs::read_to_string(site.join(&app.reg_file))?;
    if reg_src.contains(&format!("jobs::{module}::KIND")) || reg_src.contains(&rust_str(kind)) {
        bail!(
            "{} already registers the job {kind}",
            app.reg_file.display()
        );
    }
    let mut fields = parse_fields(specs, "i64")?;
    if fields.is_empty() {
        fields = parse_fields(&["id:i64".into()], "i64")?;
    }
    let (decl, names, json, lit) = rust_struct(&fields)?;
    let format: String = fields
        .iter()
        .map(|f| format!(" {0}={{{0}:?}}", f.name))
        .collect();
    let vars = [
        ("kind", kind),
        ("module", module.as_str()),
        ("payload_fields", decl.as_str()),
        ("payload_names", names.as_str()),
        ("payload_format", format.as_str()),
        ("sample_json", json.as_str()),
        ("sample_rust", lit.as_str()),
    ];
    let mut plan = Plan::default();
    plan.create(format!("src/jobs/{module}.rs"), fill("job/job.rs", &vars));
    add_module(
        &mut plan,
        site,
        &app,
        &mut reg_src,
        "jobs",
        &module,
        "The site's job handlers, registered in the app with `App::job`.",
    )?;
    reg_src = register(
        &reg_src,
        &format!(".job(jobs::{module}::KIND, jobs::{module}::run)"),
    )?;
    plan.edit(app.reg_file.clone(), reg_src);
    plan.next = vec![
        "cargo test".into(),
        format!("queue it: cx.enqueue(jobs::{module}::KIND, serde_json::json!({{...}}))"),
    ];
    Ok(plan)
}

fn mailer_plan(site: &Path, name: &str, specs: &[String]) -> Result<Plan> {
    let name = ident("mailer", name)?;
    let fields = parse_fields(specs, "string")?;
    let (decl, _, _, lit) = rust_struct(&fields)?;
    let label_ = label(&name);
    let lower = label_.to_lowercase();
    let mut text_fields = String::new();
    let mut html_fields = String::new();
    for f in &fields {
        rust_field(f)?;
        let l = label(&f.name);
        text_fields.push_str(&format!("{l}: {{{{ {} }}}}\n", f.name));
        html_fields.push_str(&format!("<p>{l}: {{{{ {} }}}}</p>\n", f.name));
    }
    let strukt = camel(&name);
    let app = if is_app_crate(site) {
        Some(app_crate(site, "a mailer")?)
    } else {
        None
    };
    let dead_code = match &app {
        Some(a) if !a.lib => {
            "#![allow(dead_code)] // Until a handler or job of this binary sends it.\n"
        }
        _ => "",
    };
    let vars = [
        ("name", name.as_str()),
        ("Label", label_.as_str()),
        ("label", lower.as_str()),
        ("text_fields", text_fields.as_str()),
        ("html_fields", html_fields.as_str()),
        ("Struct", strukt.as_str()),
        ("data_fields", decl.as_str()),
        ("sample_rust", lit.as_str()),
        ("dead_code", dead_code),
    ];
    let mut plan = Plan::default();
    plan.create(
        format!("templates/mail/{name}.txt"),
        fill("mailer/mail.txt", &vars),
    );
    plan.create(
        format!("templates/mail/{name}.html"),
        fill("mailer/mail.html", &vars),
    );
    match app {
        Some(app) => {
            app.need_dep(
                "serde",
                "serde = { version = \"1\", features = [\"derive\"] }",
                "a mailer",
            )?;
            let mut reg_src = std::fs::read_to_string(site.join(&app.reg_file))?;
            plan.create(
                format!("src/mailers/{name}.rs"),
                fill("mailer/sender.rs", &vars),
            );
            add_module(
                &mut plan,
                site,
                &app,
                &mut reg_src,
                "mailers",
                &name,
                "The site's emails: templates in templates/mail/, senders here.",
            )?;
            plan.edit(app.reg_file.clone(), reg_src);
            plan.next = vec![
                "cargo test".into(),
                format!(
                    "send it: mailers::{name}::send(mailer, to, name, &{strukt} {{ ... }}) (the \
                     engine needs the `mail` feature)"
                ),
            ];
        }
        None => {
            plan.next = vec![format!(
                "send it with Mailer::send(to, name, \"{name}\", data); `engine generate app` \
                 makes the site an app crate, then this generator also writes a sender"
            )];
        }
    }
    Ok(plan)
}

/// Engine features `generate app` accepts.
const FEATURES: &[&str] = &[
    "static",
    "db-postgres",
    "content",
    "publish",
    "request",
    "auth",
    "roles",
    "admin",
    "workflow",
    "forms",
    "jobs",
    "mail",
    "media",
    "search",
    "api",
    "oidc",
    "s3",
    "payments",
    "visual-editor",
    "cms",
    "app",
];

fn app_plan(
    site: &Path,
    name: Option<&str>,
    engine_path: Option<&Path>,
    features: &[String],
) -> Result<Plan> {
    let (cfg, _) = builder::load_config(site)?;
    let folder = std::fs::canonicalize(site)?
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("site")
        .to_string();
    let raw = name.unwrap_or(&folder).to_ascii_lowercase();
    let mut krate: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if !krate.starts_with(|c: char| c.is_ascii_lowercase()) {
        krate = format!("site-{krate}");
    }
    if ["engine", "engine-cli", "test", "build"].contains(&krate.as_str()) {
        bail!("the crate name `{krate}` is taken; pass --name");
    }
    for f in features {
        if !FEATURES.contains(&f.as_str()) {
            bail!(
                "--features: `{f}` is not an engine feature ({})",
                FEATURES.join(", ")
            );
        }
    }
    let features = features
        .iter()
        .map(|f| toml_str(f))
        .collect::<Vec<_>>()
        .join(", ");
    let engine_dep = match engine_path {
        Some(p) => format!(
            "{{ path = {}, features = [{features}] }}",
            toml_str(&p.display().to_string())
        ),
        None => {
            let v = env!("CARGO_PKG_VERSION");
            let minor = v.rsplit_once('.').map_or(v, |(a, _)| a);
            format!(
                "{{ version = {}, features = [{features}] }}",
                toml_str(minor)
            )
        }
    };
    let ident_ = krate.replace('-', "_");
    let label_ = if cfg.site.name.trim().is_empty() {
        label(&krate)
    } else {
        cfg.site.name.clone()
    };
    let vars = [
        ("Label", label_.as_str()),
        ("crate", krate.as_str()),
        ("crate_ident", ident_.as_str()),
        ("engine_dep", engine_dep.as_str()),
        ("bin", krate.as_str()),
    ];
    let mut plan = Plan::default();
    plan.create("Cargo.toml", fill("app/Cargo.toml", &vars));
    plan.create("src/lib.rs", fill("app/lib.rs", &vars));
    plan.create("src/main.rs", fill("app/main.rs", &vars));
    plan.create("tests/app.rs", fill("app/test.rs", &vars));
    let ignore = std::fs::read_to_string(site.join(".gitignore")).unwrap_or_default();
    if !ignore
        .lines()
        .any(|l| matches!(l.trim(), "target" | "target/" | "/target" | "/target/"))
    {
        let mut s = ignore.trim_end().to_string();
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("# Cargo's build output (the site is a Rust crate).\n/target/\n");
        plan.edit(".gitignore", s);
    }
    plan.next = vec![
        "cargo test".into(),
        "cargo run -- routes".into(),
        "engine generate handler hello".into(),
    ];
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(ident("type", "Blog-Post").ok(), None);
        assert_eq!(ident("type", "blog-post").unwrap(), "blog_post");
        assert!(ident("type", "1x").is_err());
        assert_eq!(label("order_shipped"), "Order shipped");
        assert_eq!(camel("order_shipped"), "OrderShipped");
        assert_eq!(plural("event"), "events");
        assert_eq!(plural("category"), "categories");
        assert_eq!(plural("day"), "days");
        assert_eq!(plural("box"), "boxes");
        assert_eq!(plural("class"), "classes");
    }

    #[test]
    fn fields_are_parsed() {
        let f = parse_fields(
            &[
                "title!".into(),
                "kind:select:a|b".into(),
                "n:integer".into(),
            ],
            "text",
        )
        .unwrap();
        assert_eq!(f[0].kind, "text");
        assert!(f[0].required);
        assert_eq!(f[1].arg.as_deref(), Some("a|b"));
        assert!(!f[2].required);
        assert!(parse_fields(&["a".into(), "a:text".into()], "text").is_err());
        assert_eq!(toml_list("a| b|"), r#"["a", "b"]"#);
    }

    #[test]
    fn registration_goes_after_app_new() {
        let lib = "use engine_cli::App;\n\npub fn app() -> App {\n    App::new()\n}\n";
        let out = register(lib, ".job(jobs::a::KIND, jobs::a::run)").unwrap();
        assert_eq!(
            out,
            "use engine_cli::App;\n\npub fn app() -> App {\n    App::new()\n        \
             .job(jobs::a::KIND, jobs::a::run)\n}\n"
        );
        // A one-line chain is split.
        let main = "fn main() -> ExitCode {\n    engine_cli::App::new().run()\n}\n";
        let out = register(main, ".route(\"/x/\", get(f))").unwrap();
        assert!(
            out.contains("engine_cli::App::new()\n        .route(\"/x/\", get(f))\n        .run()"),
            "{out}"
        );
        // A comment mentioning it is skipped.
        assert!(register("// App::new()\n", ".x()").is_err());
    }

    #[test]
    fn modules_are_declared_once() {
        let lib = "//! Doc.\n\nuse engine_cli::App;\n\npub fn app() {}\n";
        let one = declare_mod(lib, "pub mod jobs;");
        assert_eq!(
            one,
            "//! Doc.\n\nuse engine_cli::App;\n\npub mod jobs;\n\npub fn app() {}\n"
        );
        let two = declare_mod(&one, "pub mod handlers;");
        assert!(two.contains("pub mod jobs;\npub mod handlers;\n"), "{two}");
        assert_eq!(declare_mod(&two, "pub mod jobs;"), two);
        assert_eq!(
            declare_mod("//! Doc.\n\n", "pub mod a;"),
            "//! Doc.\n\npub mod a;\n"
        );
        // A multi-line `use`.
        let m = declare_mod("use a::{\n    b,\n};\nfn x() {}\n", "mod jobs;");
        assert_eq!(m, "use a::{\n    b,\n};\n\nmod jobs;\nfn x() {}\n");
    }

    #[test]
    fn every_generator_template_is_embedded() {
        for f in [
            "app/Cargo.toml",
            "app/lib.rs",
            "app/main.rs",
            "app/test.rs",
            "handler/get.rs",
            "handler/json.rs",
            "handler/page.html",
            "handler/test.rs",
            "job/job.rs",
            "mailer/mail.txt",
            "mailer/mail.html",
            "mailer/sender.rs",
            "type/item.html",
            "type/list.html",
            "route/page.html",
            "form/page.html",
        ] {
            assert!(GENERATORS.get_file(f).is_some(), "{f}");
        }
    }
}
