//! Domain prompt-draft tests: the frozen dialog output, unknown-choice
//! errors, and the protocol-once rule.
use super::prompt_draft::{
    build_prompt_text, DraftSection, PresetEntry, ProjectEntry, PromptCatalog, PromptContext,
    PromptDraft, PromptDraftError, SkillEntry,
};

const GOLDEN: &str = include_str!("prompt_draft/golden_two_sections.txt");
const GOLDEN_ATTACHMENTS: &str = include_str!("prompt_draft/golden_attachments.txt");

/// The FR4 draft: two sections, a preset, two skills and a project.
fn golden_draft() -> PromptDraft {
    PromptDraft {
        sections: vec![
            DraftSection {
                title: "instruction_1".to_string(),
                content: "do the golden thing".to_string(),
            },
            DraftSection {
                title: "goal_1".to_string(),
                content: "ship the golden feature".to_string(),
            },
        ],
        preset: Some("golden".to_string()),
        skills: vec![
            "skill:golden-alpha".to_string(),
            "canopy:golden-beta".to_string(),
        ],
        project: Some("golden-proj".to_string()),
        attachments: Vec::new(),
    }
}

/// The catalog the golden draft resolves against. Every value is literal —
/// nothing here is computed with the code under test, so the golden
/// comparison pins the assembly instead of echoing it.
fn golden_catalog() -> PromptCatalog {
    PromptCatalog {
        presets: vec![PresetEntry {
            name: "golden".to_string(),
            preview: "You are the golden reviewer.".to_string(),
            content: "You are the golden reviewer.\nCheck every line twice.".to_string(),
        }],
        skills: vec![
            SkillEntry {
                name: "golden-alpha".to_string(),
                label: "skill:golden-alpha".to_string(),
            },
            SkillEntry {
                name: "golden-beta".to_string(),
                label: "canopy:golden-beta".to_string(),
            },
        ],
        projects: vec![ProjectEntry {
            hash: "golden01".to_string(),
            name: "golden-proj".to_string(),
            path: "/golden/proj".to_string(),
            block: "name: golden-proj\nworkdir_hash: golden01\npath: /golden/proj\ndescription: The golden project\ntags: gold,demo\nindexed_at: 1700000000"
                .to_string(),
        }],
    }
}

#[test]
fn golden_two_sections_equal_the_frozen_dialog_output() {
    let ctx = PromptContext {
        catalog: golden_catalog(),
        ..PromptContext::default()
    };
    let text = build_prompt_text(&golden_draft(), &ctx).expect("golden draft builds");
    assert_eq!(text, GOLDEN);
}

#[test]
fn golden_attachments_append_last_as_escaped_image_paths() {
    let mut draft = golden_draft();
    draft.attachments = vec![
        "/home/u/shots/first.png".to_string(),
        "/home/u/my & shots/second.png".to_string(),
    ];
    let ctx = PromptContext {
        catalog: golden_catalog(),
        ..PromptContext::default()
    };
    let text = build_prompt_text(&draft, &ctx).expect("golden draft builds");
    assert_eq!(text, GOLDEN_ATTACHMENTS);
}

#[test]
fn attachments_stay_last_behind_resolved_resources() {
    // Resolved resources replace the raw section and close the body — the
    // attachments block must still render after them, not before.
    let draft = PromptDraft {
        sections: vec![DraftSection {
            title: "instruction_1".to_string(),
            content: "do it".to_string(),
        }],
        attachments: vec!["/home/u/shots/a.png".to_string()],
        ..PromptDraft::default()
    };
    let ctx = PromptContext {
        resources: vec!["path: /golden/file\nkind: file".to_string()],
        ..PromptContext::default()
    };
    let text = build_prompt_text(&draft, &ctx).expect("builds");
    let resources_at = text
        .find("# [RESOURCES]")
        .expect("resolved resources render");
    let attachments_at = text
        .find("# [ATTACHMENTS]: Images")
        .expect("attachments render");
    assert!(
        attachments_at > resources_at,
        "attachments must close the body: {text}"
    );
    assert!(text.contains("<image path=\"/home/u/shots/a.png\"/>"));
}

#[test]
fn unknown_preset_names_the_choice() {
    let draft = PromptDraft {
        preset: Some("no-such-preset".to_string()),
        ..PromptDraft::default()
    };
    let err = build_prompt_text(&draft, &PromptContext::default()).unwrap_err();
    assert_eq!(
        err,
        PromptDraftError::UnknownPreset("no-such-preset".to_string())
    );
    assert!(err.to_string().contains("no-such-preset"));
}

#[test]
fn unknown_skill_names_the_choice() {
    let draft = PromptDraft {
        skills: vec!["skill:no-such-skill".to_string()],
        ..PromptDraft::default()
    };
    let err = build_prompt_text(&draft, &PromptContext::default()).unwrap_err();
    assert_eq!(
        err,
        PromptDraftError::UnknownSkill("skill:no-such-skill".to_string())
    );
    assert!(err.to_string().contains("skill:no-such-skill"));
}

#[test]
fn unknown_project_names_the_choice() {
    let draft = PromptDraft {
        project: Some("no-such-project".to_string()),
        ..PromptDraft::default()
    };
    let err = build_prompt_text(&draft, &PromptContext::default()).unwrap_err();
    assert_eq!(
        err,
        PromptDraftError::UnknownProject("no-such-project".to_string())
    );
    assert!(err.to_string().contains("no-such-project"));
}

#[test]
fn preset_with_unbound_placeholder_refuses_naming_it() {
    let catalog = PromptCatalog {
        presets: vec![PresetEntry {
            name: "needs-var".to_string(),
            preview: String::new(),
            content: "do {{thing}} now".to_string(),
        }],
        ..PromptCatalog::default()
    };
    let draft = PromptDraft {
        preset: Some("needs-var".to_string()),
        ..PromptDraft::default()
    };
    let ctx = PromptContext {
        catalog,
        ..PromptContext::default()
    };
    let err = build_prompt_text(&draft, &ctx).unwrap_err();
    assert!(matches!(err, PromptDraftError::UnrenderablePreset { .. }));
    let message = err.to_string();
    assert!(message.contains("needs-var"), "must name it: {message}");
    assert!(message.contains("thing"), "must name it: {message}");
}

#[test]
fn skill_resolves_by_bare_name_to_its_label() {
    let catalog = PromptCatalog {
        skills: vec![SkillEntry {
            name: "alpha".to_string(),
            label: "skill:alpha".to_string(),
        }],
        ..PromptCatalog::default()
    };
    let draft = PromptDraft {
        skills: vec!["alpha".to_string()],
        ..PromptDraft::default()
    };
    let ctx = PromptContext {
        catalog,
        ..PromptContext::default()
    };
    let text = build_prompt_text(&draft, &ctx).expect("bare name resolves");
    assert!(
        text.contains("    skill:alpha\n"),
        "the label must render as the tool line: {text}"
    );
}

#[test]
fn protocol_appears_exactly_once_with_flag_and_section() {
    let draft = PromptDraft {
        sections: vec![DraftSection {
            title: "canopy_protocol_1".to_string(),
            content: "stale body, never rendered".to_string(),
        }],
        ..PromptDraft::default()
    };
    let ctx = PromptContext {
        protocol_included: true,
        ..PromptContext::default()
    };
    let text = build_prompt_text(&draft, &ctx).expect("protocol draft builds");
    let block = crate::domain::sandbox::canopy_protocol_block();
    assert_eq!(text.matches(block).count(), 1);
    assert!(text.starts_with("<system>\n"));
}

#[test]
fn canopy_protocol_section_alone_includes_the_block_once() {
    let draft = PromptDraft {
        sections: vec![DraftSection {
            title: "canopy_protocol".to_string(),
            content: String::new(),
        }],
        ..PromptDraft::default()
    };
    let text = build_prompt_text(&draft, &PromptContext::default()).expect("builds");
    let block = crate::domain::sandbox::canopy_protocol_block();
    assert_eq!(text.matches(block).count(), 1);
}

#[test]
fn no_protocol_block_by_default() {
    let draft = PromptDraft {
        sections: vec![DraftSection {
            title: "instruction_1".to_string(),
            content: "do it".to_string(),
        }],
        ..PromptDraft::default()
    };
    let text = build_prompt_text(&draft, &PromptContext::default()).expect("builds");
    assert!(!text.contains(crate::domain::sandbox::canopy_protocol_block()));
    assert!(!text.contains("<system>"));
}
