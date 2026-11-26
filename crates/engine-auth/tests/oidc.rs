//! Account rules of single sign-on against the test database (Postgres or
//! SQLite; skipped without ENGINE_TEST_DATABASE_URL): an identity is never
//! matched to an account by its email address, new accounts only when the
//! provider allows it, linking and unlinking rules, blocked accounts, and
//! single-use, expiring sign-in attempts.

#![cfg(all(feature = "oidc", feature = "roles"))]

use std::sync::Arc;

use engine_auth::oidc::jwt::IdClaims;
use engine_auth::oidc::{LinkOutcome, Purpose, SignInOutcome, UnlinkOutcome};
use engine_auth::{Auth, AuthConfig, UserChanges, notify::MemoryNotifier};
use engine_db::testing::TestDb;
use engine_db::{Timestamp, migrate};

const CORP: &str = "https://corp.example";
const PARTNER: &str = "https://partner.example";

fn config() -> AuthConfig {
    let t: toml::Table = toml::from_str(&format!(
        r#"
        default_roles = ["editor"]
        [[oidc]]
        name = "corp"
        label = "Corp"
        issuer = "{CORP}"
        client_id = "w"
        create_accounts = true
        [[oidc]]
        name = "partner"
        label = "Partner"
        issuer = "{PARTNER}"
        client_id = "w"
        "#
    ))
    .unwrap();
    AuthConfig::from_site(Some(&t)).unwrap()
}

fn claims(issuer: &str, sub: &str, email: &str, verified: bool) -> IdClaims {
    IdClaims {
        issuer: issuer.into(),
        subject: sub.into(),
        email: Some(email.into()),
        email_verified: verified,
        name: Some("Someone".into()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn identities_never_take_over_accounts() {
    let Some(t) = TestDb::create("oidc_accounts").await else {
        return;
    };
    migrate::apply(&t.db, &engine_auth::migrations(t.backend()))
        .await
        .unwrap();
    let auth = Auth::new(
        t.db.clone(),
        config(),
        "Test",
        "http://localhost",
        Arc::new(MemoryNotifier::default()),
    );
    let corp = auth.oidc_providers()[0].clone();
    let partner = auth.oidc_providers()[1].clone();
    assert!(corp.creates_accounts() && !partner.creates_accounts());
    let admin = auth
        .create_user(
            "admin@example.org",
            "Admin",
            Some("admin password, long"),
            &["admin".into()],
        )
        .await
        .unwrap();
    let bob = auth
        .create_user("bob@example.org", "Bob", Some("bob's password, long"), &[])
        .await
        .unwrap();
    let ip = Some("192.0.2.1");

    // A provider identity with Bob's (verified) address does not sign in
    // as Bob, however the address is written, and nothing is linked.
    for email in ["bob@example.org", "BOB@Example.org", " bob@example.org "] {
        let c = claims(CORP, "attacker", email, true);
        assert_eq!(
            auth.oidc_sign_in(&corp, &c, ip).await.unwrap(),
            SignInOutcome::EmailInUse,
            "{email}"
        );
    }
    assert!(auth.identities(bob.id).await.unwrap().is_empty());
    assert_eq!(auth.user_count().await.unwrap(), 2);
    // The administrator neither, and not through a provider that makes no
    // accounts.
    let c = claims(PARTNER, "x", "admin@example.org", true);
    assert_eq!(
        auth.oidc_sign_in(&partner, &c, ip).await.unwrap(),
        SignInOutcome::NotLinked
    );
    let c = claims(PARTNER, "new", "new@example.org", true);
    assert_eq!(
        auth.oidc_sign_in(&partner, &c, ip).await.unwrap(),
        SignInOutcome::NotLinked
    );

    // New accounts need a verified address.
    let c = claims(CORP, "carol", "carol@example.org", false);
    assert_eq!(
        auth.oidc_sign_in(&corp, &c, ip).await.unwrap(),
        SignInOutcome::NoVerifiedEmail
    );
    let mut c = claims(CORP, "carol", "x", true);
    c.email = None;
    assert_eq!(
        auth.oidc_sign_in(&corp, &c, ip).await.unwrap(),
        SignInOutcome::NoVerifiedEmail
    );

    // A new identity with a fresh verified address gets an account with
    // the default roles (never admin), no password, a verified address.
    let c = claims(CORP, "alice-sub", "Alice@Example.org", true);
    let SignInOutcome::Created(alice) = auth.oidc_sign_in(&corp, &c, ip).await.unwrap() else {
        panic!("no account made");
    };
    assert_eq!(alice.email, "alice@example.org");
    assert_eq!(alice.roles, ["editor"]);
    assert!(alice.email_verified);
    assert!(!auth.has_password(alice.id).await.unwrap());
    assert!(!alice.roles.iter().any(|r| r == "admin"));
    // Signing in again finds the same account, even when the provider now
    // reports another address.
    let c2 = claims(CORP, "alice-sub", "alice.new@example.org", true);
    assert_eq!(
        auth.oidc_sign_in(&corp, &c2, ip).await.unwrap(),
        SignInOutcome::Ok(alice.clone())
    );
    // The same subject at another issuer is another identity.
    let c3 = claims(PARTNER, "alice-sub", "alice@example.org", true);
    assert_eq!(
        auth.oidc_sign_in(&partner, &c3, ip).await.unwrap(),
        SignInOutcome::NotLinked
    );

    // Bob links Corp from his profile; then it signs him in.
    let bc = claims(CORP, "bob-sub", "bob@example.org", true);
    assert_eq!(
        auth.oidc_link(bob.id, &corp, &bc).await.unwrap(),
        LinkOutcome::Linked
    );
    assert_eq!(
        auth.oidc_link(bob.id, &corp, &bc).await.unwrap(),
        LinkOutcome::AlreadyLinked
    );
    let SignInOutcome::Ok(u) = auth.oidc_sign_in(&corp, &bc, ip).await.unwrap() else {
        panic!("bob not signed in");
    };
    assert_eq!(u.id, bob.id);
    let ids = auth.identities(bob.id).await.unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(
        (ids[0].provider.as_str(), ids[0].subject.as_str()),
        ("corp", "bob-sub")
    );
    // Alice's identity cannot be linked to Bob, nor a second Corp identity.
    assert_eq!(
        auth.oidc_link(bob.id, &corp, &c).await.unwrap(),
        LinkOutcome::OtherAccount
    );
    let other = claims(CORP, "bob-second", "bob@example.org", true);
    assert_eq!(
        auth.oidc_link(bob.id, &corp, &other).await.unwrap(),
        LinkOutcome::IssuerInUse
    );

    // Unlinking never leaves an account without a way in.
    assert_eq!(
        auth.oidc_unlink(alice.id, CORP).await.unwrap(),
        UnlinkOutcome::LastWay
    );
    assert_eq!(auth.identities(alice.id).await.unwrap().len(), 1);
    assert_eq!(
        auth.oidc_unlink(alice.id, PARTNER).await.unwrap(),
        UnlinkOutcome::NotLinked
    );
    assert_eq!(
        auth.oidc_unlink(bob.id, CORP).await.unwrap(),
        UnlinkOutcome::Unlinked
    );
    assert_eq!(
        auth.oidc_sign_in(&corp, &bc, ip).await.unwrap(),
        SignInOutcome::EmailInUse
    );

    // Blocked accounts do not sign in.
    auth.oidc_link(bob.id, &corp, &bc).await.unwrap();
    let bob = auth.user(bob.id).await.unwrap().unwrap();
    auth.update_user(
        bob.id,
        bob.version,
        &UserChanges {
            active: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        auth.oidc_sign_in(&corp, &bc, ip).await.unwrap(),
        SignInOutcome::Blocked
    );
    let _ = admin;

    // Failed attempts count towards the address's limit (30 by default);
    // past it even a linked identity is refused.
    for _ in 0..30 {
        let c = claims(PARTNER, "nobody", "nobody@example.org", true);
        auth.oidc_sign_in(&partner, &c, Some("192.0.2.9"))
            .await
            .unwrap();
    }
    assert_eq!(
        auth.oidc_sign_in(&corp, &c2, Some("192.0.2.9"))
            .await
            .unwrap(),
        SignInOutcome::Limited
    );
    t.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_attempts_are_single_use_and_expire() {
    let Some(t) = TestDb::create("oidc_states").await else {
        return;
    };
    migrate::apply(&t.db, &engine_auth::migrations(t.backend()))
        .await
        .unwrap();
    let auth = Auth::new(
        t.db.clone(),
        config(),
        "Test",
        "http://localhost",
        Arc::new(MemoryNotifier::default()),
    );
    let s = auth
        .oidc_begin("corp", Purpose::Login, None, "/next/")
        .await
        .unwrap();
    assert_ne!(s.state, s.nonce);
    assert_ne!(s.verifier, s.cookie);
    // Another browser's (or a made-up) cookie finds nothing.
    assert!(
        auth.oidc_claim(&engine_auth::secret::token())
            .await
            .unwrap()
            .is_none()
    );
    assert!(auth.oidc_claim("not a token").await.unwrap().is_none());
    let p = auth.oidc_claim(&s.cookie).await.unwrap().unwrap();
    assert_eq!((p.provider.as_str(), p.next.as_str()), ("corp", "/next/"));
    assert_eq!(p.state, s.state);
    assert_eq!(p.purpose, Purpose::Login);
    // Replayed: gone.
    assert!(auth.oidc_claim(&s.cookie).await.unwrap().is_none());

    // Expired attempts are not accepted.
    let s = auth
        .oidc_begin("corp", Purpose::Link, None, "/account/profile/")
        .await
        .unwrap();
    engine_db::sql::query("UPDATE engine_oidc_states SET expires_at = $1")
        .bind(Timestamp::now().minus_minutes(1))
        .execute(&t.db)
        .await
        .unwrap();
    assert!(auth.oidc_claim(&s.cookie).await.unwrap().is_none());
    t.cleanup().await;
}
