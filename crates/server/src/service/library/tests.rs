#![cfg(test)]
use super::*;
use crate::state::Ctx;
use std::env;

fn fixture() -> (PathBuf, Shared, i64) {
    let root = env::temp_dir().join(format!("synco-snapshot-{}-{}", util::now_ms(), util::r4()));
    let source = root.join("source");
    fs::create_dir_all(source.join("projects/1")).unwrap();
    let ctx = Ctx::new(source.clone(), source.clone(), repo::db::open(&source).unwrap());
    let pid = repo::projects::create(&ctx, "snapshot").unwrap();
    let iid = repo::images::insert(&ctx, pid, "a.png", "projects/1/a.png", 2, 2).unwrap();
    fs::write(source.join("projects/1/a.png"), crate::img::codec::encode_png(&stitch_core::Rgba::new(2,2))).unwrap();
    fs::create_dir_all(source.join("runtime/cache")).unwrap();
    fs::write(source.join("runtime/cache/thumb.jpg"), [1,2]).unwrap();
    repo::images::set_derived(&ctx, iid, Some("runtime/cache/thumb.jpg"), None).unwrap();
    repo::adjust::put(&ctx, iid, &photoedit_core::EditOps::default().to_json()).unwrap();
    (root, ctx, iid)
}

#[tokio::test]
async fn wal_snapshot_is_complete_and_rebuilds_skipped_cache_without_changing_source() {
    let (root, ctx, iid) = fixture();
    assert!(ctx.data.join("app.db-wal").exists());
    fs::create_dir(ctx.data.join("workflows")).unwrap();
    fs::write(ctx.data.join("workflows/inside.json"), "{}").unwrap();
    let original_path = util::rel_slash(&ctx.data.join("workflows/inside.json"));
    repo::settings::put(&ctx, "workflow_path", &original_path).unwrap();
    repo::settings::put(&ctx, &format!("workflow_profile:{original_path}"), "{\"bindings\":[],\"output_node\":\"51\"}").unwrap();
    repo::settings::put(&ctx, &format!("workflow_roles:{original_path}"), "{\"out_final\":\"51\"}").unwrap();
    let copied = copy(&ctx, &root.join("copied")).await.unwrap();
    assert!(copied.files >= 2);
    assert_eq!(fs::read(copied.path.join("projects/1/a.png")).unwrap(), fs::read(ctx.data.join("projects/1/a.png")).unwrap());
    assert!(!copied.path.join("runtime").exists());
    assert!(!copied.path.join("app.db-wal").exists());
    let target = Ctx::new(copied.path.clone(), copied.path.clone(), repo::db::open(&copied.path).unwrap());
    assert!(repo::adjust::ops_of(&target, iid).unwrap().is_some());
    assert_eq!(repo::images::by_id(&target, iid).unwrap().unwrap().thumb_path, None);
    assert!(repo::images::by_id(&ctx, iid).unwrap().unwrap().thumb_path.is_some());
    let next_path = repo::settings::get(&target, "workflow_path").unwrap();
    assert!(Path::new(&next_path).is_file());
    assert!(repo::settings::get(&target, &format!("workflow_profile:{next_path}")).is_some());
    assert!(repo::settings::get(&target, &format!("workflow_roles:{next_path}")).is_some());
    assert_eq!(repo::settings::get(&ctx, "workflow_path").unwrap(), original_path);
    drop(target); drop(ctx); fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn busy_requests_and_active_jobs_prevent_copy_and_leave_no_target() {
    let (root, ctx, iid) = fixture();
    let request = ctx.work().enter().await;
    assert!(copy(&ctx, &root.join("copied")).await.is_err());
    assert!(!root.join("copied").exists());
    drop(request);
    let job = repo::results::insert_cloud(&ctx, iid, 1, "p", "{}", "mock", None).unwrap();
    assert!(copy(&ctx, &root.join("copied")).await.is_err());
    repo::results::mark_failed(&ctx, job, "srv.result.cancelled", serde_json::Value::Null);
    assert!(copy(&ctx, &root.join("copied")).await.is_ok());
    drop(ctx); fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_external_database_or_file_change_rejects_publication_and_cleans_stage() {
    let (root, ctx, _) = fixture();
    let draft = prepare(&ctx, &root.join("copied")).unwrap();
    draft.copy_files(&ctx).unwrap();
    let external = Ctx::new(ctx.data.clone(), ctx.data.clone(), repo::db::open(&ctx.data).unwrap());
    repo::projects::rename(&external, 1, "changed").unwrap();
    assert!(draft.publish(&ctx).is_err());
    assert!(!root.join("copied").exists());
    let draft = prepare(&ctx, &root.join("copied")).unwrap();
    draft.copy_files(&ctx).unwrap();
    fs::write(ctx.data.join("projects/1/a.png"), "changed").unwrap();
    assert!(draft.publish(&ctx).is_err());
    assert!(fs::read_dir(&root).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".synco-copy-")));
    drop(external); drop(ctx); fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn descendants_junction_aliases_and_nonempty_targets_are_rejected() {
    let (root, ctx, _) = fixture();
    assert!(copy(&ctx, &ctx.data.join("backup/nested")).await.is_err());
    assert!(!ctx.data.join("backup").exists());
    #[cfg(windows)] {
        let alias = root.join("alias");
        assert!(std::process::Command::new("cmd").args(["/C","mklink","/J"]).arg(&alias).arg(&ctx.data).output().unwrap().status.success());
        assert!(copy(&ctx, &alias.join("backup")).await.is_err());
        fs::remove_dir(&alias).unwrap();
    }
    fs::create_dir_all(root.join("occupied")).unwrap();
    fs::write(root.join("occupied/keep"), "keep").unwrap();
    assert!(copy(&ctx, &root.join("occupied")).await.is_err());
    assert_eq!(fs::read_to_string(root.join("occupied/keep")).unwrap(), "keep");
    drop(ctx); fs::remove_dir_all(root).unwrap();
}
