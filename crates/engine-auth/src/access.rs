//! What a signed-in user may do: per content type actions, per-field
//! access, taxonomy and user management.
//!
//! [`Access`] is always compiled. With the `roles` feature it is the union of
//! the user's roles' [`Grants`]; without it every active account gets
//! [`Access::full`]. The admin checks it on every request, server-side,
//! whatever the form showed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// An action on items of a content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    View,
    Create,
    Edit,
    Delete,
    /// Change an item's status (publish or unpublish).
    Publish,
}

impl Action {
    pub const ALL: [Action; 5] = [
        Action::View,
        Action::Create,
        Action::Edit,
        Action::Delete,
        Action::Publish,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Action::View => "view",
            Action::Create => "create",
            Action::Edit => "edit",
            Action::Delete => "delete",
            Action::Publish => "publish",
        }
    }
}

/// Access to one field. Ordered: `None < Read < Write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FieldAccess {
    /// Hidden: not shown, not in revisions, not writable.
    None,
    /// Shown, not writable.
    Read,
    /// Shown and writable (the default).
    #[default]
    Write,
}

impl FieldAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldAccess::None => "none",
            FieldAccess::Read => "read",
            FieldAccess::Write => "write",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(FieldAccess::None),
            "read" => Some(FieldAccess::Read),
            "write" => Some(FieldAccess::Write),
            _ => None,
        }
    }
}

/// A role's rights on one content type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeGrant {
    #[serde(default)]
    pub view: bool,
    #[serde(default)]
    pub create: bool,
    #[serde(default)]
    pub edit: bool,
    #[serde(default)]
    pub delete: bool,
    #[serde(default)]
    pub publish: bool,
    /// Fields with less than write access.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, FieldAccess>,
}

impl TypeGrant {
    pub fn allows(&self, a: Action) -> bool {
        match a {
            Action::View => self.view,
            Action::Create => self.create,
            Action::Edit => self.edit,
            Action::Delete => self.delete,
            Action::Publish => self.publish,
        }
    }

    fn set(&mut self, a: Action, on: bool) {
        match a {
            Action::View => self.view = on,
            Action::Create => self.create = on,
            Action::Edit => self.edit = on,
            Action::Delete => self.delete = on,
            Action::Publish => self.publish = on,
        }
    }

    /// Union with another grant (most permissive wins).
    fn union(&mut self, other: &TypeGrant) {
        // A field restricted in only one of the grants is as open as the
        // other grant leaves it (absent = write).
        let names: Vec<String> = self
            .fields
            .keys()
            .chain(other.fields.keys())
            .cloned()
            .collect();
        // A grant that cannot write at all contributes at most `read`, so
        // a view-only role does not open fields another role restricts.
        let eff = |g: &TypeGrant, n: &str| {
            let l = g.fields.get(n).copied().unwrap_or_default();
            if g.edit || g.create {
                l
            } else {
                l.min(FieldAccess::Read)
            }
        };
        let mut merged = BTreeMap::new();
        for n in names {
            let m = eff(self, &n).max(eff(other, &n));
            if m != FieldAccess::Write {
                merged.insert(n, m);
            }
        }
        self.fields = merged;
        for a in Action::ALL {
            self.set(a, self.allows(a) || other.allows(a));
        }
    }
}

/// What a role grants (stored as JSON in `engine_roles.grants`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    /// By content type; `*` applies to types without their own entry.
    #[serde(default)]
    pub types: BTreeMap<String, TypeGrant>,
    /// Rename and delete taxonomy terms.
    #[serde(default)]
    pub taxonomy: bool,
    /// Manage accounts and roles.
    #[serde(default)]
    pub users: bool,
}

impl Grants {
    /// The grant for a type: its own entry, else `*`.
    pub fn for_type(&self, type_name: &str) -> Option<&TypeGrant> {
        self.types.get(type_name).or_else(|| self.types.get("*"))
    }
}

/// The effective rights of one user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Access {
    admin: bool,
    grants: Grants,
    /// Names of the roles this came from (for display).
    pub roles: Vec<String>,
}

impl Access {
    /// Everything (administrators; every account without `roles`).
    pub fn full() -> Self {
        Access {
            admin: true,
            grants: Grants::default(),
            roles: vec!["admin".into()],
        }
    }

    /// Nothing.
    pub fn none() -> Self {
        Access::default()
    }

    /// Combine roles: `(name, is_admin, grants)`.
    pub fn from_roles<'a>(roles: impl IntoIterator<Item = (&'a str, bool, &'a Grants)>) -> Self {
        let mut acc = Access::none();
        let mut types: BTreeMap<String, TypeGrant> = BTreeMap::new();
        let mut wildcards: Vec<&Grants> = Vec::new();
        let mut all: Vec<&Grants> = Vec::new();
        for (name, is_admin, g) in roles {
            acc.roles.push(name.to_string());
            acc.admin |= is_admin;
            acc.grants.taxonomy |= g.taxonomy;
            acc.grants.users |= g.users;
            all.push(g);
            if g.types.contains_key("*") {
                wildcards.push(g);
            }
        }
        // Resolve every named type per role first (a role's own entry
        // replaces its `*`), then union across roles.
        let mut names: Vec<&String> = all.iter().flat_map(|g| g.types.keys()).collect();
        names.sort();
        names.dedup();
        for n in names.into_iter().filter(|n| n.as_str() != "*") {
            let mut merged: Option<TypeGrant> = None;
            for g in &all {
                if let Some(tg) = g.for_type(n) {
                    match &mut merged {
                        Some(m) => m.union(tg),
                        None => merged = Some(tg.clone()),
                    }
                }
            }
            if let Some(m) = merged {
                types.insert(n.clone(), m);
            }
        }
        let mut star: Option<TypeGrant> = None;
        for g in wildcards {
            let tg = &g.types["*"];
            match &mut star {
                Some(s) => s.union(tg),
                None => star = Some(tg.clone()),
            }
        }
        if let Some(s) = star {
            types.insert("*".into(), s);
        }
        acc.grants.types = types;
        acc
    }

    pub fn is_admin(&self) -> bool {
        self.admin
    }

    fn grant(&self, type_name: &str) -> Option<&TypeGrant> {
        self.grants.for_type(type_name)
    }

    /// Whether the user may do `a` on items of `type_name`. Every action
    /// needs `view`.
    pub fn can(&self, type_name: &str, a: Action) -> bool {
        self.admin || self.grant(type_name).is_some_and(|g| g.view && g.allows(a))
    }

    /// Access to a field when doing `a` (`Create` or `Edit`; anything else
    /// is read-only at most).
    pub fn field(&self, type_name: &str, field: &str, a: Action) -> FieldAccess {
        if self.admin {
            return FieldAccess::Write;
        }
        let Some(g) = self.grant(type_name).filter(|g| g.view) else {
            return FieldAccess::None;
        };
        let level = g.fields.get(field).copied().unwrap_or_default();
        let writable = matches!(a, Action::Create | Action::Edit) && g.allows(a);
        if writable {
            level
        } else {
            level.min(FieldAccess::Read)
        }
    }

    pub fn can_manage_taxonomy(&self) -> bool {
        self.admin || self.grants.taxonomy
    }

    pub fn can_manage_users(&self) -> bool {
        self.admin || self.grants.users
    }

    /// Whether the admin has anything to show this user.
    pub fn can_use_admin(&self) -> bool {
        self.admin
            || self.grants.taxonomy
            || self.grants.users
            || self.grants.types.values().any(|g| g.view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(json: &str) -> Grants {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn full_and_none() {
        let f = Access::full();
        assert!(f.can("article", Action::Delete));
        assert_eq!(f.field("article", "x", Action::Edit), FieldAccess::Write);
        assert!(f.can_manage_users() && f.can_use_admin());
        let n = Access::none();
        assert!(!n.can("article", Action::View));
        assert_eq!(n.field("article", "x", Action::Edit), FieldAccess::None);
        assert!(!n.can_use_admin());
    }

    #[test]
    fn wildcard_type_override_and_fields() {
        let editor = grants(r#"{"types": {"*": {"view": true, "edit": true, "create": true}}}"#);
        let restricted = grants(
            r#"{"types": {"article": {"view": true, "edit": true,
                 "fields": {"author": "read", "notes": "none"}}}}"#,
        );
        let a = Access::from_roles([("contributor", false, &restricted)]);
        assert!(a.can("article", Action::Edit));
        assert!(!a.can("article", Action::Publish));
        assert!(!a.can("author", Action::View));
        assert_eq!(
            a.field("article", "title", Action::Edit),
            FieldAccess::Write
        );
        assert_eq!(
            a.field("article", "author", Action::Edit),
            FieldAccess::Read
        );
        assert_eq!(a.field("article", "notes", Action::Edit), FieldAccess::None);
        assert_eq!(
            a.field("article", "title", Action::Create),
            FieldAccess::Read,
            "no create right"
        );
        // Union: the editor role leaves `author` writable.
        let both = Access::from_roles([
            ("contributor", false, &restricted),
            ("editor", false, &editor),
        ]);
        assert_eq!(
            both.field("article", "author", Action::Edit),
            FieldAccess::Write
        );
        assert!(both.can("author", Action::Create));
        assert_eq!(both.roles, ["contributor", "editor"]);
    }

    #[test]
    fn own_entry_replaces_wildcard_within_a_role() {
        let g = grants(
            r#"{"types": {"*": {"view": true, "edit": true},
                          "settings": {"view": true}}, "taxonomy": true}"#,
        );
        let a = Access::from_roles([("r", false, &g)]);
        assert!(a.can("article", Action::Edit));
        assert!(!a.can("settings", Action::Edit));
        assert_eq!(
            a.field("settings", "tagline", Action::Edit),
            FieldAccess::Read
        );
        assert!(a.can_manage_taxonomy() && !a.can_manage_users());
        assert!(serde_json::from_str::<Grants>(r#"{"typo": 1}"#).is_err());
    }
}
