//! Integration tests for indexing, metadata and search.

use crate::db::{path_key, Db, Scope, SortKey};
use crate::query::Filter;
use crate::scan::scan_root;
use std::fs;
use std::path::Path;

fn setup() -> (tempfile::TempDir, tempfile::TempDir, Db, String) {
    let data = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let root = files.path();
    fs::create_dir_all(root.join("photos/2024")).unwrap();
    fs::write(root.join("photos/2024/beach.jpg"), b"x").unwrap();
    fs::write(root.join("photos/2024/mountain.png"), b"xx").unwrap();
    fs::write(root.join("photos/notes.txt"), b"hello").unwrap();
    fs::write(root.join("report.pdf"), b"pdf").unwrap();
    fs::write(root.join(".hidden"), b"h").unwrap();
    let db = Db::open_in(data.path()).unwrap();
    let key = path_key(root);
    db.add_root(root).unwrap();
    scan_root(&db.path, &key, None).unwrap();
    (data, files, db, key)
}

fn names(db: &Db, scope: &Scope, q: &str) -> Vec<String> {
    let mut v: Vec<String> = db
        .search(scope, &Filter::parse(q), SortKey::Name, false, 1000)
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    v.sort();
    v
}

fn id_of(db: &Db, name: &str) -> i64 {
    db.search(&Scope::All, &Filter::default(), SortKey::Name, false, 1000)
        .unwrap()
        .into_iter()
        .find(|r| r.name == name)
        .unwrap()
        .id
}

#[test]
fn scan_indexes_files_and_skips_hidden() {
    let (_d, _f, db, _root) = setup();
    assert_eq!(
        names(&db, &Scope::All, ""),
        vec![
            "2024",
            "beach.jpg",
            "mountain.png",
            "notes.txt",
            "photos",
            "report.pdf"
        ]
    );
}

#[test]
fn folder_scope_direct_and_recursive() {
    let (_d, _f, db, root) = setup();
    let photos = path_key(&Path::new(&root).join("photos"));
    let direct = Scope::Folder {
        path: photos.clone(),
        recursive: false,
    };
    assert_eq!(names(&db, &direct, ""), vec!["2024", "notes.txt"]);
    let rec = Scope::Folder {
        path: photos,
        recursive: true,
    };
    assert_eq!(
        names(&db, &rec, "is:file"),
        vec!["beach.jpg", "mountain.png", "notes.txt"]
    );
}

#[test]
fn tags_stars_labels_comments_filter() {
    let (_d, _f, db, _root) = setup();
    let beach = id_of(&db, "beach.jpg");
    let mountain = id_of(&db, "mountain.png");
    db.add_tag(&[beach, mountain], "旅行").unwrap();
    db.add_tag(&[beach], "#海").unwrap();
    db.set_star(&[beach], 4).unwrap();
    db.set_label(&[mountain], 4).unwrap();
    db.set_comment(&[mountain], "富士山の写真").unwrap();

    assert_eq!(
        names(&db, &Scope::All, "#旅行"),
        vec!["beach.jpg", "mountain.png"]
    );
    assert_eq!(names(&db, &Scope::All, "#旅行 -#海"), vec!["mountain.png"]);
    assert_eq!(names(&db, &Scope::All, "star:3"), vec!["beach.jpg"]);
    assert_eq!(names(&db, &Scope::All, "label:緑"), vec!["mountain.png"]);
    assert_eq!(names(&db, &Scope::All, "富士山"), vec!["mountain.png"]);
    assert_eq!(
        names(&db, &Scope::All, "ext:jpg,pdf"),
        vec!["beach.jpg", "report.pdf"]
    );
    assert_eq!(
        names(&db, &Scope::All, "is:untagged is:file"),
        vec!["notes.txt", "report.pdf"]
    );

    let tags = db.tag_counts().unwrap();
    assert_eq!(tags, vec![("旅行".to_string(), 2), ("海".to_string(), 1)]);

    // rename into an existing tag merges
    db.rename_tag("海", "旅行").unwrap();
    assert_eq!(db.tag_counts().unwrap(), vec![("旅行".to_string(), 2)]);
}

#[test]
fn like_wildcards_are_literal() {
    let (_d, _f, db, _root) = setup();
    assert!(names(&db, &Scope::All, "%").is_empty());
    assert!(names(&db, &Scope::All, "_").is_empty());
}

#[test]
fn virtual_folders_manual_and_smart() {
    let (_d, _f, db, _root) = setup();
    let pdf = id_of(&db, "report.pdf");
    let notes = id_of(&db, "notes.txt");
    let p = db.create_profile("仕事", None).unwrap();
    db.add_to_profile(p, &[pdf, notes]).unwrap();
    assert_eq!(
        names(&db, &Scope::Profile(p), ""),
        vec!["notes.txt", "report.pdf"]
    );
    assert_eq!(
        names(&db, &Scope::Profile(p), "ext:pdf"),
        vec!["report.pdf"]
    );
    db.remove_from_profile(p, &[notes]).unwrap();
    assert_eq!(db.profile_count(p).unwrap(), 1);
    assert_eq!(db.profiles_of(pdf).unwrap(), vec![p]);

    db.set_star(&[notes], 5).unwrap();
    let smart = db.create_profile("お気に入り", Some("star:5")).unwrap();
    assert_eq!(names(&db, &Scope::Profile(smart), ""), vec!["notes.txt"]);
}

#[test]
fn rescan_keeps_metadata_of_deleted_files_as_missing() {
    let (_d, files, db, root) = setup();
    let pdf = id_of(&db, "report.pdf");
    db.add_tag(&[pdf], "大事").unwrap();
    fs::remove_file(files.path().join("report.pdf")).unwrap();
    fs::remove_file(files.path().join("photos/notes.txt")).unwrap();
    fs::write(files.path().join("new.md"), b"n").unwrap();
    let (_count, removed) = scan_root(&db.path, &root, None).unwrap();
    assert_eq!(removed, 1); // notes.txt had no metadata
    assert_eq!(names(&db, &Scope::All, "is:missing"), vec!["report.pdf"]);
    assert!(names(&db, &Scope::All, "").contains(&"new.md".to_string()));
    // coming back clears the flag
    fs::write(files.path().join("report.pdf"), b"pdf").unwrap();
    scan_root(&db.path, &root, None).unwrap();
    assert!(names(&db, &Scope::All, "is:missing").is_empty());
    assert_eq!(names(&db, &Scope::All, "#大事"), vec!["report.pdf"]);
}

#[test]
fn rename_folder_moves_children_and_keeps_tags() {
    let (_d, files, db, root) = setup();
    let beach = id_of(&db, "beach.jpg");
    db.add_tag(&[beach], "夏").unwrap();
    let dir = id_of(&db, "2024");
    db.rename(dir, "2025").unwrap();
    assert!(files.path().join("photos/2025/beach.jpg").exists());
    let f = db.file(beach).unwrap().unwrap();
    assert_eq!(
        f.path,
        path_key(&files.path().join("photos").join("2025").join("beach.jpg"))
    );
    assert_eq!(f.tags, vec!["夏"]);
    // a rescan must not duplicate or lose anything
    scan_root(&db.path, &root, None).unwrap();
    assert_eq!(names(&db, &Scope::All, "#夏"), vec!["beach.jpg"]);
    let in_2025 = Scope::Folder {
        path: path_key(&files.path().join("photos").join("2025")),
        recursive: false,
    };
    assert_eq!(names(&db, &in_2025, ""), vec!["beach.jpg", "mountain.png"]);
}

#[test]
fn drag_and_drop_registration_outside_roots() {
    let (_d, _f, db, _root) = setup();
    let other = tempfile::tempdir().unwrap();
    let p = other.path().join("dropped.zip");
    fs::write(&p, b"z").unwrap();
    let id = db.register_path(&p).unwrap();
    let again = db.register_path(&p).unwrap();
    assert_eq!(id, again);
    let pid = db.create_profile("集め", None).unwrap();
    db.add_to_profile(pid, &[id]).unwrap();
    assert_eq!(names(&db, &Scope::Profile(pid), ""), vec!["dropped.zip"]);
}
