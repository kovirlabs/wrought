//! Media store against the test database (Postgres or SQLite; skipped
//! without ENGINE_TEST_DATABASE_URL).

use std::io::Cursor;

use engine_db::migrate;
use engine_db::testing::TestDb;
use engine_media::{MediaConfig, MediaError, MediaStore, Upload};
use image::{DynamicImage, ImageFormat};

fn jpeg_with_gps(w: u32, h: u32) -> Vec<u8> {
    let img = DynamicImage::new_rgb8(w, h);
    let mut plain = Vec::new();
    img.write_to(&mut Cursor::new(&mut plain), ImageFormat::Jpeg)
        .unwrap();
    let payload = b"Exif\0\0GPS 51.4778N 0.0015W";
    let len = (payload.len() + 2) as u16;
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&plain[2..]);
    out
}

fn up<'a>(bytes: &'a [u8], filename: &'a str) -> Upload<'a> {
    Upload {
        bytes,
        filename,
        alt: "A test image",
        private: false,
        uploaded_by: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_are_checked_cleaned_resized_and_versioned() {
    let Some(t) = TestDb::create("media_uploads").await else {
        return;
    };
    migrate::apply(&t.db, &engine_media::migrations(t.backend()))
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cfg = MediaConfig {
        max_mb: 1,
        ..Default::default()
    };
    let store = MediaStore::new(t.db.clone(), cfg, dir.path().join("media"));

    // A photo with location data, under a hostile name.
    let photo = jpeg_with_gps(1400, 700);
    let m = store
        .upload(up(&photo, "../../../etc/Harbour at Dawn.php.jpg"))
        .await
        .unwrap();
    assert_eq!(m.name, "harbour-at-dawn-php");
    assert_eq!(m.ext, "jpg");
    assert_eq!((m.width, m.height), (Some(1400), Some(700)));
    assert_eq!(m.variants, [320, 640, 960, 1280]);
    let url = m.url().unwrap();
    assert_eq!(url, format!("/media/{}/harbour-at-dawn-php.jpg", m.id));
    let on_disk = std::fs::read(store.path(&m)).unwrap();
    assert!(!on_disk.windows(3).any(|w| w == b"GPS"), "EXIF stripped");
    assert!(store.path(&m).starts_with(dir.path().join("media/public")));
    for w in &m.variants {
        let v = store.folder(&m).join(m.variant_file(*w));
        let img = image::open(&v).unwrap();
        assert_eq!(img.width(), *w);
    }
    assert!(m.srcset().unwrap().contains("-320.jpg 320w"));

    // Content decides the type, not the name.
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'/>";
    let err = store.upload(up(svg, "innocent.png")).await.unwrap_err();
    assert!(
        matches!(err, MediaError::Refused(ref m) if m.contains("SVG")),
        "{err}"
    );
    let html = b"<!doctype html><script>alert(1)</script>";
    assert!(matches!(
        store.upload(up(html, "page.jpg")).await.unwrap_err(),
        MediaError::Refused(_)
    ));
    let exe = b"MZ\x90\x00\x03\x00\x00\x00";
    assert!(matches!(
        store.upload(up(exe, "setup.pdf")).await.unwrap_err(),
        MediaError::Refused(_)
    ));
    // Too large.
    let big = vec![0xFFu8; 2 * 1024 * 1024];
    assert!(matches!(
        store.upload(up(&big, "big.jpg")).await.unwrap_err(),
        MediaError::TooLarge { max_mb: 1 }
    ));
    // A PDF is kept as it is.
    let pdf = b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n";
    let doc = store.upload(up(pdf, "Price list.PDF")).await.unwrap();
    assert_eq!(
        (doc.ext.as_str(), doc.mime.as_str()),
        ("pdf", "application/pdf")
    );
    assert!(doc.srcset().is_none());
    // Private files have no public URL and live apart.
    let private = store
        .upload(Upload {
            private: true,
            ..up(&photo, "id-card.jpg")
        })
        .await
        .unwrap();
    assert!(private.url().is_none());
    assert!(
        store
            .path(&private)
            .starts_with(dir.path().join("media/private"))
    );
    assert_eq!(store.count(false, Some("image")).await.unwrap(), 1);
    assert_eq!(store.count(false, None).await.unwrap(), 2);
    assert_eq!(store.count(true, None).await.unwrap(), 1);

    // Descriptions are version-checked.
    let m2 = store
        .set_alt(m.id, m.version, "Boats at dawn")
        .await
        .unwrap();
    assert_eq!(m2.version, m.version + 1);
    assert!(matches!(
        store.set_alt(m.id, m.version, "stale").await.unwrap_err(),
        MediaError::Conflict { .. }
    ));
    // Deleting removes the files too.
    assert!(matches!(
        store.delete(m.id, m.version).await.unwrap_err(),
        MediaError::Conflict { .. }
    ));
    store.delete(m.id, m2.version).await.unwrap();
    assert!(!store.folder(&m).exists());
    assert!(store.get(m.id).await.unwrap().is_none());
    t.cleanup().await;
}
