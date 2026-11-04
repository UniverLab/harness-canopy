//! Catalog tests: `compose` builds hub-side exactly like the domain,
//! unknown choices answer `bad_request` naming them, `catalog` lists the
//! pickers' sources, and draft `prompt` delivers like text `prompt`.

use super::{
    connect_client, recv_json, send_json, serve_state, terminal_spawn, test_db, test_hub, KillGuard,
};
use crate::domain::project::Project;
use crate::domain::prompt_draft::{build_prompt_text, DraftSection, PromptContext, PromptDraft};
use crate::sessions::catalog::{
    catalog_frame, composed_frame, CatalogCli, CatalogData, CatalogPreset, CatalogProject,
    CatalogSkill,
};
use crate::sessions::protocol::{parse_client_frame, parse_host_frame, ClientFrame, HostFrame};
use crate::sessions::ws::SessionsState;

#[tokio::test]
async fn compose_matches_domain_build() {
    let (db, _db_dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    // No preset/skills/project keys at all: the draft defaults must apply.
    send_json(
        &mut client,
        &serde_json::json!({
            "t": "compose", "req": "c1",
            "draft": {"sections": [
                {"title": "instruction_1", "content": "do it live"},
                {"title": "goal", "content": "win"},
            ]},
        }),
    )
    .await;
    let composed = recv_json(&mut client, "composed").await;
    assert_eq!(composed["t"], "composed");
    assert_eq!(composed["req"], "c1");

    let draft = PromptDraft {
        sections: vec![
            DraftSection {
                title: "instruction_1".to_string(),
                content: "do it live".to_string(),
            },
            DraftSection {
                title: "goal".to_string(),
                content: "win".to_string(),
            },
        ],
        ..PromptDraft::default()
    };
    let expected = build_prompt_text(&draft, &PromptContext::default()).expect("builds");
    assert_eq!(composed["text"], expected.as_str());
    server.abort();
}

#[tokio::test]
async fn compose_unknown_preset_is_bad_request_naming_it() {
    let (db, _db_dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({
            "t": "compose", "req": "c2",
            "draft": {"sections": [], "preset": "no-such-preset-xyz"},
        }),
    )
    .await;
    let error = recv_json(&mut client, "error").await;
    assert_eq!(error["t"], "error");
    assert_eq!(error["code"], "bad_request");
    assert_eq!(error["req"], "c2");
    assert!(
        error["message"]
            .as_str()
            .expect("message")
            .contains("no-such-preset-xyz"),
        "must name the choice: {error}"
    );
    server.abort();
}

#[test]
fn prompt_text_xor_draft() {
    let both = parse_client_frame(
        r#"{"t": "prompt", "req": "r", "id": "x", "text": "hi", "draft": {"sections": []}}"#,
    )
    .unwrap_err();
    assert!(
        both.message.contains("text"),
        "must name it: {}",
        both.message
    );
    assert!(
        both.message.contains("draft"),
        "must name it: {}",
        both.message
    );

    let neither = parse_client_frame(r#"{"t": "prompt", "req": "r", "id": "x"}"#).unwrap_err();
    assert!(
        neither.message.contains("text"),
        "must name it: {}",
        neither.message
    );
    assert!(
        neither.message.contains("draft"),
        "must name it: {}",
        neither.message
    );

    // `null` counts as absent, matching the other optional fields.
    assert!(matches!(
        parse_client_frame(
            r#"{"t": "prompt", "req": "r", "id": "x", "text": null, "draft": {"sections": []}}"#
        ),
        Ok(ClientFrame::PromptDraft { .. })
    ));

    let invalid = parse_client_frame(
        r#"{"t": "prompt", "req": "r", "id": "x", "draft": {"sections": [{"title": 1}]}}"#,
    )
    .unwrap_err();
    assert!(
        invalid.message.contains("draft"),
        "must name it: {}",
        invalid.message
    );

    let missing = parse_client_frame(r#"{"t": "compose", "req": "r"}"#).unwrap_err();
    assert!(
        missing.message.contains("draft"),
        "must name it: {}",
        missing.message
    );
}

#[test]
fn compose_and_catalog_shapes_roundtrip() {
    assert!(matches!(
        parse_client_frame(
            r#"{"t": "compose", "req": "r", "draft": {"sections": [{"title": "goal", "content": "g"}]}}"#
        ),
        Ok(ClientFrame::Compose { .. })
    ));
    assert!(matches!(
        parse_client_frame(r#"{"t": "catalog", "req": "r"}"#),
        Ok(ClientFrame::Catalog { .. })
    ));

    let composed = composed_frame("c1", "hello");
    let frame = parse_host_frame(&composed).expect("composed parses");
    assert!(matches!(frame, HostFrame::Composed { .. }));
    assert_eq!(frame.req(), Some("c1"));

    let data = CatalogData {
        clis: vec![CatalogCli {
            cli: "claude".to_string(),
            label: "claude".to_string(),
            models: vec!["m1".to_string()],
            yolo: true,
        }],
        projects: vec![CatalogProject {
            name: "p".to_string(),
            path: "/p".to_string(),
        }],
        presets: vec![CatalogPreset {
            name: "r".to_string(),
            preview: "pv".to_string(),
        }],
        skills: vec![CatalogSkill {
            name: "s".to_string(),
            label: "skill:s".to_string(),
        }],
    };
    match parse_host_frame(&catalog_frame("c2", &data)) {
        Ok(HostFrame::Catalog { req, data }) => {
            assert_eq!(req, "c2");
            assert_eq!(data.clis.len(), 1);
            assert_eq!(data.clis[0].models, vec!["m1".to_string()]);
            assert!(data.clis[0].yolo, "catalog yolo must survive the wire");
            assert_eq!(data.projects.len(), 1);
            assert_eq!(data.presets.len(), 1);
            assert_eq!(data.skills.len(), 1);
        }
        other => panic!("expected a catalog frame, got {other:?}"),
    }
}

#[test]
fn catalog_lists_temp_dir_sources() {
    let (db, _db_dir) = test_db();
    db.upsert_project(&Project {
        hash: "cat0001".to_string(),
        name: "cat-proj".to_string(),
        path: "/cat/proj".to_string(),
        description: None,
        tags: None,
        indexed_at: None,
        created_at: 0,
    })
    .unwrap();

    let workdir = tempfile::tempdir().expect("workdir");
    let skill_dir = workdir
        .path()
        .join(".agents")
        .join("skills")
        .join("tempskill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: tempskill\n---\n# tempskill\n",
    )
    .unwrap();

    let canopy_dir = tempfile::tempdir().expect("canopy dir");
    let prompts = canopy_dir.path().join("prompts");
    std::fs::create_dir_all(&prompts).unwrap();
    std::fs::write(prompts.join("gold.md"), "First line preview.\n\nBody here.").unwrap();

    let data = CatalogData::load(&db, workdir.path(), canopy_dir.path()).expect("loads");
    assert!(
        data.projects
            .iter()
            .any(|p| p.name == "cat-proj" && p.path == "/cat/proj"),
        "projects must list the registered row: {:?}",
        data.projects
    );
    assert!(
        data.presets
            .iter()
            .any(|p| p.name == "gold" && p.preview == "First line preview."),
        "presets must list the temp prompt: {:?}",
        data.presets
    );
    assert!(
        data.skills
            .iter()
            .any(|s| s.name == "tempskill" && s.label == "skill:tempskill"),
        "skills must list the temp skill: {:?}",
        data.skills
    );
    // The dialog's three fallback slugs guarantee a non-empty CLI list even
    // when nothing is detected.
    assert!(!data.clis.is_empty());
    for cli in &data.clis {
        assert!(!cli.cli.is_empty() && !cli.label.is_empty());
    }
}

#[tokio::test]
async fn catalog_over_hub_answers_wire_catalog() {
    let (db, _db_dir) = test_db();
    db.upsert_project(&Project {
        hash: "cathub0001".to_string(),
        name: "cat-hub-proj".to_string(),
        path: "/cat/hub/proj".to_string(),
        description: None,
        tags: None,
        indexed_at: None,
        created_at: 0,
    })
    .unwrap();

    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({"t": "catalog", "req": "cat-hub-1"}),
    )
    .await;
    let catalog = recv_json(&mut client, "catalog").await;
    assert_eq!(catalog["t"], "catalog");
    assert_eq!(catalog["req"], "cat-hub-1");

    // Hub wiring: `handle_catalog` reads the hub's own db, so the row we
    // inserted above must appear in the wire answer.
    let projects = catalog["projects"].as_array().expect("projects array");
    assert!(
        projects
            .iter()
            .any(|p| p["name"] == "cat-hub-proj" && p["path"] == "/cat/hub/proj"),
        "hub catalog must list the db project: {catalog}"
    );

    // Wire shape: clis carry cli/label/models, presets and skills are arrays.
    let clis = catalog["clis"].as_array().expect("clis array");
    assert!(!clis.is_empty(), "clis must be non-empty: {catalog}");
    for cli in clis {
        assert!(
            cli["cli"].is_string() && cli["label"].is_string(),
            "cli row shape: {catalog}"
        );
        assert!(cli["models"].is_array(), "cli models array: {catalog}");
    }
    assert!(catalog["presets"].is_array());
    assert!(catalog["skills"].is_array());

    // The wire answer must parse as a host `Catalog` frame echoing the req.
    match parse_host_frame(&catalog.to_string()) {
        Ok(HostFrame::Catalog { req, data }) => {
            assert_eq!(req, "cat-hub-1");
            assert!(data.projects.iter().any(|p| p.name == "cat-hub-proj"));
        }
        other => panic!("expected a catalog frame, got {other:?}"),
    }
    server.abort();
}

#[tokio::test]
async fn prompt_draft_delivered_to_live_session_answers_ok() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("sh", &workdir.path().to_string_lossy()),
    );
    let id = session.id.clone();
    drop(session);

    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({
            "t": "prompt", "req": "p1", "id": id,
            "draft": {"sections": [{"title": "instruction_1", "content": "echo remote-draft-ok"}]},
        }),
    )
    .await;
    let ok = recv_json(&mut client, "ok").await;
    assert_eq!(ok["t"], "ok");
    assert_eq!(ok["req"], "p1");
    server.abort();
}
