//! Render tests of the built-in component library: every example renders,
//! text is escaped, markup works without JavaScript, assets load only where
//! a component needs them, and sites override components.

use std::path::Path;
use std::sync::Arc;

use minijinja::{Environment, Value, context};

use crate::builder::{BuildOptions, build_site};
use crate::components::Catalogue;
use crate::content::Content;
use crate::templates::environment;

fn env_for(site: Option<&Path>) -> Environment<'static> {
    #[allow(unused_mut)]
    let mut env = environment(
        site.map(Path::to_path_buf),
        Arc::new(Content::default()),
        "https://example.org".into(),
        crate::templates::core_dirs(),
        Arc::new(Catalogue::load(&[], site).unwrap()),
    );
    #[cfg(feature = "request")]
    crate::appui::register(&mut env);
    env
}

fn ctx() -> Value {
    context! {
        site => context! {
            name => "Site",
            language => "en",
            nav => vec![
                context! { label => "Home", url => "/" },
                context! { label => "Docs", url => "/docs/" },
            ],
        },
        page => context! { url => "/docs/x/", title => "X" },
        request => context! { path => "/docs/x/", args => context! {} },
    }
}

fn render(env: &Environment<'static>, src: &str, args: Value) -> String {
    let mut m = crate::templates::value_map(&ctx());
    m.extend(crate::templates::value_map(&args));
    env.render_str(src, Value::from(m)).unwrap()
}

const HOSTILE: &str = "<script>alert(1)</script>\"'";

#[test]
fn every_example_renders() {
    let env = env_for(None);
    let cat = Catalogue::load(&[], None).unwrap();
    for c in cat.components() {
        let out = env
            .render_str(
                &format!("{{{{ component(\"{}\", ex) }}}}", c.name),
                context! { ex => c.example_value(), ..ctx() },
            )
            .unwrap_or_else(|e| panic!("{}: {e:#}", c.name));
        assert!(!out.contains("<script>alert"), "{}", c.name);
        if !["footer", "header", "prev_next", "pagefind_search"].contains(&c.name.as_str()) {
            assert!(!out.trim().is_empty(), "{} rendered nothing", c.name);
        }
    }
}

#[test]
fn text_arguments_are_escaped() {
    let env = env_for(None);
    let h = Value::from(HOSTILE);
    for (name, args) in [
        (
            "section",
            context! { title => h.clone(), text => h.clone(), label => h.clone(), id => h.clone() },
        ),
        (
            "menu",
            context! { label => h.clone(), items => vec![context! { label => h.clone(), url => h.clone(), children => vec![context!{ label => h.clone(), url => "/a/" }] }] },
        ),
        (
            "mobile_menu",
            context! { label => h.clone(), id => h.clone() },
        ),
        (
            "tabs",
            context! { label => h.clone(), id => h.clone(), items => vec![context! { label => h.clone(), text => h.clone() }] },
        ),
        ("rich_text", context! { text => h.clone() }),
        (
            "timeline",
            context! { title => h.clone(), items => vec![context! { title => h.clone(), text => h.clone(), date => h.clone(), href => h.clone() }] },
        ),
        (
            "video_embed",
            context! { url => "https://youtu.be/aqz-KE-bpKQ", title => h.clone(), caption => h.clone(), consent => true },
        ),
        (
            "video_embed",
            context! { url => "/v/a.mp4", title => h.clone(), captions => h.clone() },
        ),
        (
            "columns",
            context! { columns => vec![h.clone()], ratio => h.clone() },
        ),
        (
            "grid",
            context! { items => vec![h.clone()], min => h.clone() },
        ),
        (
            "stack",
            context! { items => vec![h.clone()], gap => h.clone() },
        ),
        ("spacer", context! { size => h.clone() }),
        (
            "container",
            context! { body => h.clone(), width => h.clone() },
        ),
    ] {
        let out = render(
            &env,
            &format!("{{{{ component(\"{name}\", a) }}}}"),
            context! { a => args },
        );
        assert!(!out.contains("<script>alert"), "{name}: {out}");
        assert!(!out.contains("\"'"), "{name}: unescaped quote in {out}");
    }
    // Levels and other attribute-shaped arguments only take known values.
    let out = render(
        &env,
        r#"{{ component("section", title="T", level="1 onclick=alert(1)") }}"#,
        context! {},
    );
    assert!(out.contains("<h2>T</h2>"), "{out}");
}

#[test]
fn rich_text_sources() {
    let env = env_for(None);
    let out = render(
        &env,
        r#"{{ component("rich_text", markdown="Hi *x* <b>no</b> [l](javascript:alert(1))") }}"#,
        context! {},
    );
    assert!(
        out.contains("<em>x</em>") && out.contains("&lt;b&gt;"),
        "{out}"
    );
    assert!(!out.contains("javascript:"), "{out}");
    let out = render(
        &env,
        r#"{{ component("rich_text", text="one\n\ntwo") }}|{{ component("rich_text", html=h) }}"#,
        context! { h => Value::from_safe_string("<p>made</p>".into()) },
    );
    assert!(
        out.contains("<p>one</p>") && out.contains("<p>two</p>"),
        "{out}"
    );
    assert!(out.contains("<p>made</p>"), "{out}");
}

#[test]
fn navigation_markup_works_without_javascript() {
    let env = env_for(None);
    let menu = render(
        &env,
        r#"{{ component("menu", items=[{"label": "A", "url": "/a/"}, {"label": "B", "url": "/b/", "children": [{"label": "C", "url": "/docs/x/"}]}]) }}"#,
        context! {},
    );
    assert!(menu.contains(r#"<nav aria-label="Main">"#), "{menu}");
    assert!(menu.contains("<details>\n<summary>B</summary>"), "{menu}");
    assert!(
        menu.contains(r#"<a href="/docs/x/" aria-current="page">C</a>"#),
        "{menu}"
    );
    assert!(!menu.contains("<script"), "{menu}");

    // The header uses the mobile menu: a native popover, no script.
    let header = render(&env, r#"{{ component("header") }}"#, context! {});
    assert!(
        header.contains(r#"<button type="button" popovertarget="site-menu">"#),
        "{header}"
    );
    assert!(
        header.contains(r#"<div popover id="site-menu">"#),
        "{header}"
    );
    assert!(
        header.contains(r#"<a href="/docs/" aria-current="page">Docs</a>"#),
        "{header}"
    );
    assert!(!header.contains("<script"), "{header}");

    // Tabs: the ARIA pattern TagAction switches; every panel is in the
    // HTML and none is hidden, so all show without JavaScript.
    let tabs = render(
        &env,
        r#"{{ component("tabs", id="t", label="Pick", selected=2, items=[{"label": "One", "text": "1"}, {"label": "Two", "text": "2"}]) }}"#,
        context! {},
    );
    assert!(
        tabs.contains(r#"<div role="tablist" aria-label="Pick">"#),
        "{tabs}"
    );
    assert!(
        tabs.contains(r#"id="t-1-tab" aria-controls="t-1" aria-selected="false""#),
        "{tabs}"
    );
    assert!(
        tabs.contains(r#"id="t-2-tab" aria-controls="t-2" aria-selected="true""#),
        "{tabs}"
    );
    assert!(
        tabs.contains(
            r#"<section id="t-2" role="tabpanel" aria-labelledby="t-2-tab" tabindex="0">"#
        ),
        "{tabs}"
    );
    assert!(
        !tabs.contains("hidden") && !tabs.contains("<script"),
        "{tabs}"
    );
}

#[test]
fn video_embed_loads_its_script_only_with_consent() {
    let env = env_for(None);
    let plain = render(
        &env,
        r#"{{ component("video_embed", url="https://vimeo.com/76979871", title="Film") }}"#,
        context! {},
    );
    assert!(
        plain.contains(
            r#"<iframe src="https://player.vimeo.com/video/76979871?dnt=1" title="Film""#
        ),
        "{plain}"
    );
    assert!(!plain.contains("<script"), "{plain}");
    let consent = render(
        &env,
        r#"{{ component("video_embed", url="https://youtu.be/aqz-KE-bpKQ", title="Film", consent=true) }}"#,
        context! {},
    );
    assert!(
        consent.contains(
            r#"<a href="https://www.youtube.com/watch?v=aqz-KE-bpKQ">Watch “Film” on YouTube</a>"#
        ),
        "{consent}"
    );
    assert!(
        !consent.contains("<iframe"),
        "no third-party request before consent: {consent}"
    );
    assert!(
        consent.contains(r#"<script type="module" src="/_engine/components/video_embed-"#),
        "{consent}"
    );
    let file = render(
        &env,
        r#"{{ component("video_embed", url="/media/tour.mp4", title="Tour", captions="/media/tour.vtt") }}"#,
        context! {},
    );
    assert!(
        file.contains("<video controls")
            && file.contains(r#"<track kind="captions" src="/media/tour.vtt" srclang="en""#),
        "{file}"
    );
    let bad = render(
        &env,
        r#"{{ component("video_embed", url="javascript:alert(1)", title="x") }}"#,
        context! {},
    );
    assert!(bad.trim().is_empty(), "{bad}");
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn built_pages_load_component_assets_once_and_only_where_used() {
    let d = tempfile::tempdir().unwrap();
    let site = d.path();
    write(
        site,
        "site.toml",
        "[site]\nname = \"T\"\n\n[[routes]]\npath = \"/videos/\"\ntemplate = \"videos.html\"\n\n[[routes]]\npath = \"/plain/\"\ntemplate = \"plain.html\"\n",
    );
    write(
        site,
        "content/_index.md",
        "+++\ntitle = \"Home\"\n+++\nHi.\n",
    );
    write(
        site,
        "templates/videos.html",
        r#"{% extends "base.html" %}{% block main %}
{{ component("video_embed", url="https://youtu.be/aqz-KE-bpKQ", title="A", consent=true) }}
{{ component("video_embed", url="https://youtu.be/aqz-KE-bpKQ", title="B", consent=true) }}
{{ component("stack", items=["a", "b"]) }}{{ component("columns", columns=["a", "b"]) }}
{% endblock %}"#,
    );
    write(
        site,
        "templates/plain.html",
        r#"{% extends "base.html" %}{% block main %}{{ component("tabs", label="L", items=[{"label": "x", "text": "y"}]) }}{% endblock %}"#,
    );
    let out = d.path().join("out");
    build_site(
        site,
        &BuildOptions {
            output: Some(out.clone()),
            drafts: false,
        },
    )
    .unwrap();
    let videos = std::fs::read_to_string(out.join("videos/index.html")).unwrap();
    let scripts = videos
        .matches("<script type=\"module\" src=\"/_engine/components/video_embed-")
        .count();
    assert_eq!(scripts, 1, "{videos}");
    assert_eq!(
        videos.matches("/_engine/components/layout-").count(),
        1,
        "{videos}"
    );
    let plain = std::fs::read_to_string(out.join("plain/index.html")).unwrap();
    assert!(!plain.contains("/_engine/components/"), "{plain}");
    let home = std::fs::read_to_string(out.join("index.html")).unwrap();
    assert!(!home.contains("/_engine/components/"), "{home}");
    // The files exist where the tags point.
    let cat = Catalogue::load(&[], None).unwrap();
    for a in cat.assets() {
        assert!(
            out.join(a.path.trim_start_matches('/')).is_file(),
            "{}",
            a.path
        );
    }
}

#[test]
fn site_overrides_a_new_component_and_keeps_its_assets_function() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "components/tabs.html",
        "<p>my tabs: {{ label }}</p>{{ component_assets(\"video_embed\") }}",
    );
    let env = env_for(Some(d.path()));
    let out = render(&env, r#"{{ component("tabs", label="<x>") }}"#, context! {});
    assert!(out.starts_with("<p>my tabs: &lt;x&gt;</p>"), "{out}");
    assert!(out.contains("/_engine/components/video_embed-"), "{out}");
    assert!(
        env.render_str(r#"{{ component_assets("nope") }}"#, context! {})
            .is_err()
    );
    // Partials are not components.
    assert!(
        env.render_str(r#"{{ component("_nav") }}"#, context! {})
            .is_err()
    );
}

#[test]
fn component_takes_a_map_of_arguments() {
    let env = env_for(None);
    let out = render(
        &env,
        r#"{{ component("quote", {"text": "a", "cite": "b"}, cite="c") }}"#,
        context! {},
    );
    assert!(
        out.contains("<p>a</p>") && out.contains("<figcaption>c</figcaption>"),
        "{out}"
    );
    assert!(
        env.render_str(r#"{{ component("quote", 3) }}"#, context! {})
            .is_err()
    );
}
