//! Source guard: the skill-line injection code must not hardcode any
//! harness user instruction file path; paths come only from the registry.

use std::path::Path;

#[test]
fn skill_line_constants_contain_no_hardcoded_harness_paths() {
    let instruction_file_src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/domain/instruction_file.rs"),
    )
    .expect("read src/domain/instruction_file.rs");

    // Scope to the new skill-block constants and functions (excluding
    // `offline_spec`, which already lists `.claude/CLAUDE.md` precedence).
    let consts_start = instruction_file_src
        .find("pub const SKILL_BEGIN")
        .expect("SKILL_BEGIN present");
    let consts_end = instruction_file_src[consts_start..]
        .find("pub struct InstructionSpec")
        .map(|off| consts_start + off)
        .expect("InstructionSpec present");
    let fns_start = instruction_file_src
        .find("pub fn amend_with_skill_line")
        .expect("amend_with_skill_line present");
    let fns_end = instruction_file_src[fns_start..]
        .find("pub fn resolve_target")
        .map(|off| fns_start + off)
        .expect("resolve_target present");
    let skill_section = format!(
        "{}\n{}",
        &instruction_file_src[consts_start..consts_end],
        &instruction_file_src[fns_start..fns_end]
    );

    let forbidden = [
        concat!(".claude/", "CLAUDE.md"),
        concat!(".codex/", "AGENTS.md"),
        concat!(".gemini/", "GEMINI.md"),
        concat!(".config/", "opencode"),
        concat!(".kiro/", "steering"),
    ];
    for needle in &forbidden {
        assert!(
            !skill_section.contains(needle),
            "skill-line code must not hardcode harness path {needle}"
        );
    }
}
