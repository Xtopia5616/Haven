use haven_skills::{Skill, SkillRegistry};
use std::path::Path;

/// Build a Skill through the same manifest parser and registry path as production.
pub(crate) async fn discover_skill_fixture(
    skills_root: &Path,
    name: &str,
    description: &str,
    script: Option<&str>,
) -> Skill {
    let skill_root = skills_root.join(name);
    std::fs::create_dir_all(&skill_root).expect("create skill fixture directory");
    let manifest = format!(
        "# Skill: {name}\n\n## Metadata\n- description: {description}\n- language: python\n\n## Instructions\nRun the skill.\n"
    );
    std::fs::write(skill_root.join("SKILL.md"), manifest).expect("write skill fixture manifest");
    if let Some(script) = script {
        let scripts = skill_root.join("scripts");
        std::fs::create_dir_all(&scripts).expect("create skill fixture scripts directory");
        std::fs::write(scripts.join("main.py"), script).expect("write skill fixture script");
    }

    let registry = SkillRegistry::new();
    registry
        .set_config(
            Some(skills_root.to_path_buf()),
            Some(vec![name.to_string()]),
        )
        .await
        .expect("scan skill fixture directory");
    registry
        .get_skill(name)
        .await
        .expect("discover valid skill fixture")
}
