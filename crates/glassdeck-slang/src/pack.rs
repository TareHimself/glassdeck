//! `glassdeck pack` for a scene folder: `scene.slang` plus an optional `keyboard.luau`, into a `.deckbg`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use glassdeck_luau_check::{LuauChecker, LuauFile};
use glassdeck_package::{
    Background, BackgroundWriter, Content, FORMAT_VERSION, Manifest, Origin, SCENE_API, SCENE_FRAGMENT, SCENE_PLAN, SCENE_SCRIPT,
    ScenePlan, scene_sources,
};

use crate::scene::{FRAGMENT_ENTRY, SceneShape};
use crate::{Slang, Source, Target, essl};

pub const SCENE_SOURCE: &str = "scene.slang";
pub const SCENE_KEYBOARD_SCRIPT: &str = "keyboard.luau";
const API_MODULE: &str = include_str!("../slang/glassdeck.slang");
/// The scene script API, written beside the generated `inputs.d.luau`
const SCENE_DEFINITIONS: &str = include_str!("../../glassdeck-luau/types/scene.d.luau");
/// Where packing writes a scene's Luau type definitions, inside its folder
pub const TYPES_FOLDER: &str = ".glassdeck";
/// Slang's GLSL target closest to what `essl` rewrites from.
const GLSL_PROFILE: &str = "glsl_450";

pub fn is_scene_folder(folder: &Path) -> bool {
    folder.join(SCENE_SOURCE).is_file()
}

pub fn pack_scene(slang: &Slang, luau: &LuauChecker, folder: &Path, out: &Path) -> Result<Background> {
    let source_path = folder.join(SCENE_SOURCE);
    let source = fs::read_to_string(&source_path).with_context(|| format!("reading {}", source_path.display()))?;
    let script_path = folder.join(SCENE_KEYBOARD_SCRIPT);
    let script = script_path
        .is_file()
        .then(|| fs::read_to_string(&script_path))
        .transpose()
        .with_context(|| format!("reading {}", script_path.display()))?;

    let shown_path = source_path.to_string_lossy();
    let api = Source { name: "glassdeck", path: "glassdeck.slang", text: API_MODULE };
    let scene = Source { name: "scene", path: &shown_path, text: &source };
    let shape = SceneShape::read(&slang.reflect(&[api, scene])?)?;
    let inputs = write_types(folder, &shape)?;
    if let Some(script) = &script {
        check_script(luau, script, &inputs)?;
    }
    if script.is_none() {
        shape.check_without_script()?;
    }
    let glue = shape.glue("scene", script.is_some());
    let glue_module = Source { name: "gd_glue", path: "glue.slang", text: &glue };
    let glsl = slang.compile(&[api, scene, glue_module], FRAGMENT_ENTRY, Target::Glsl, GLSL_PROFILE)?;
    let fragment = essl::fragment_to_essl(&String::from_utf8(glsl).context("Slang's GLSL isn't UTF-8")?)?;

    let plan = ScenePlan {
        api: SCENE_API,
        scale: shape.scale,
        frame_rate: shape.frame_rate,
        fragment: SCENE_FRAGMENT.into(),
        script: script.as_ref().map(|_| SCENE_SCRIPT.into()),
        inputs: shape.input_fields(),
    };
    plan.validate()?;
    let title = folder.file_name().and_then(|n| n.to_str()).unwrap_or("scene").to_owned();
    let manifest = Manifest {
        format: FORMAT_VERSION,
        title: title.clone(),
        origin: Origin::Packed { from: title, sources: scene_sources(API_MODULE, &source, script.as_deref()) },
        content: Content::Scene { plan: SCENE_PLAN.into() },
        preview: None,
    };
    let mut writer = BackgroundWriter::create(out, manifest)?;
    writer.add_bytes(SCENE_PLAN, &serde_json::to_vec_pretty(&plan)?)?;
    writer.add_bytes(SCENE_FRAGMENT, fragment.as_bytes())?;
    if let Some(script) = &script {
        writer.add_bytes(SCENE_SCRIPT, script.as_bytes())?;
    }
    writer.finish()
}

/// `.glassdeck/scene.d.luau` and `.glassdeck/inputs.d.luau`, for editors; returns the inputs' definitions for the type check.
fn write_types(folder: &Path, shape: &SceneShape) -> Result<String> {
    let types = folder.join(TYPES_FOLDER);
    fs::create_dir_all(&types).with_context(|| format!("creating {}", types.display()))?;
    let inputs = shape.inputs_definitions();
    for (name, text) in [("scene.d.luau", SCENE_DEFINITIONS), ("inputs.d.luau", inputs.as_str())] {
        let path = types.join(name);
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(inputs)
}

/// Luau's strict type check of the script against the scene API and the shader's inputs.
fn check_script(luau: &LuauChecker, script: &str, inputs: &str) -> Result<()> {
    let definitions = [LuauFile { name: "scene.d.luau", text: SCENE_DEFINITIONS }, LuauFile { name: "inputs.d.luau", text: inputs }];
    let problems = luau.check(&definitions, LuauFile { name: SCENE_KEYBOARD_SCRIPT, text: script })?;
    if !problems.is_empty() {
        bail!("{SCENE_KEYBOARD_SCRIPT} doesn't type-check:\n  {}", problems.join("\n  "));
    }
    Ok(())
}
