//! `glassdeck pack` for a canvas folder: `deck.json`, `canvas.slang`, and optional `keyboard.luau`,
//! `options.luau` and `host.luau`, into a background `.deck`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use glassdeck_luau::options::{OptionsScript, Panel};
use glassdeck_luau_check::{LuauChecker, LuauFile, definitions};
use glassdeck_package::{
    CANVAS_API, Canvas, CanvasScripts, Content, DeckSource, HOST_SCRIPT, KEYBOARD_SCRIPT, Kind, OPTIONS_SCRIPT, Origin, Package, PackageWriter,
    SHADER, SourceType, canvas_sources,
};

use crate::canvas::{FRAGMENT_ENTRY, CanvasShape};
use crate::{Slang, Source, Target, essl};

pub const CANVAS_SOURCE: &str = "canvas.slang";
const API_MODULE: &str = include_str!("../slang/glassdeck.slang");
/// Where packing writes a canvas's Luau type definitions, inside its folder
pub const TYPES_FOLDER: &str = ".glassdeck";
/// Slang's GLSL target closest to what `essl` rewrites from.
const GLSL_PROFILE: &str = "glsl_450";

pub fn is_canvas_folder(folder: &Path) -> bool {
    folder.join(CANVAS_SOURCE).is_file()
}

pub fn pack_canvas(slang: &Slang, luau: &LuauChecker, folder: &Path, out: &Path) -> Result<Package> {
    let (deck, deck_text) = DeckSource::read(folder)?;
    if deck.kind != SourceType::Background || deck.details.is_some() {
        bail!("a canvas's deck.json says \"type\": \"background\" and no details; packing works them out from {CANVAS_SOURCE}");
    }
    let source_path = folder.join(CANVAS_SOURCE);
    let source = fs::read_to_string(&source_path).with_context(|| format!("reading {}", source_path.display()))?;
    let keyboard = read_optional(folder, KEYBOARD_SCRIPT)?;
    let options = read_optional(folder, OPTIONS_SCRIPT)?;
    let host = read_optional(folder, HOST_SCRIPT)?;

    let shown_path = source_path.to_string_lossy();
    let api = Source { name: "glassdeck", path: "glassdeck.slang", text: API_MODULE };
    let canvas = Source { name: "canvas", path: &shown_path, text: &source };
    let shape = CanvasShape::read(&slang.reflect(&[api, canvas])?)?;
    let panel = match &options {
        Some(options) => OptionsScript::new(options).first_pass()?,
        None => Panel { items: Vec::new() },
    };
    let types = write_types(folder, &shape, &panel)?;
    check_scripts(luau, &types, keyboard.as_deref(), options.as_deref(), host.as_deref())?;
    if keyboard.is_none() {
        shape.check_without_script()?;
    }
    let glue = shape.glue("canvas", keyboard.is_some());
    let glue_module = Source { name: "gd_glue", path: "glue.slang", text: &glue };
    let glsl = slang.compile(&[api, canvas, glue_module], FRAGMENT_ENTRY, Target::Glsl, GLSL_PROFILE)?;
    let fragment = essl::fragment_to_essl(&String::from_utf8(glsl).context("Slang's GLSL isn't UTF-8")?)?;

    let canvas = Canvas { api: CANVAS_API, scale: shape.scale, frame_rate: shape.frame_rate, inputs: shape.input_fields() };
    canvas.validate()?;
    let from = folder.file_name().and_then(|n| n.to_str()).unwrap_or("canvas").to_owned();
    let scripts = CanvasScripts { keyboard: keyboard.as_deref(), options: options.as_deref(), host: host.as_deref() };
    let origin = Origin::Packed { from, sources: canvas_sources(&deck_text, API_MODULE, &source, scripts) };
    let mut writer = PackageWriter::create(out, deck.manifest(origin, Kind::Background(Content::Canvas(canvas))))?;
    writer.add_bytes(SHADER, fragment.as_bytes())?;
    for (entry, text) in [(KEYBOARD_SCRIPT, &keyboard), (OPTIONS_SCRIPT, &options), (HOST_SCRIPT, &host)] {
        if let Some(text) = text {
            writer.add_bytes(entry, text.as_bytes())?;
        }
    }
    writer.finish()
}

fn read_optional(folder: &Path, name: &str) -> Result<Option<String>> {
    let path = folder.join(name);
    path.is_file().then(|| fs::read_to_string(&path)).transpose().with_context(|| format!("reading {}", path.display()))
}

/// The generated definitions a canvas's scripts are checked against.
struct Types {
    inputs: String,
    options: String,
}

/// `.glassdeck/`: the canvas API and the generated `inputs.d.luau` and `options.d.luau`, for editors and the checks.
fn write_types(folder: &Path, shape: &CanvasShape, panel: &Panel) -> Result<Types> {
    let folder = folder.join(TYPES_FOLDER);
    fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    let types = Types { inputs: shape.inputs_definitions(), options: panel.definitions() };
    let files = [
        ("canvas.d.luau", definitions::CANVAS),
        ("inputs.d.luau", types.inputs.as_str()),
        ("options-api.d.luau", definitions::OPTIONS_API),
        ("options.d.luau", types.options.as_str()),
        ("common.d.luau", definitions::COMMON),
        ("host.d.luau", definitions::HOST),
    ];
    for (name, text) in files {
        let path = folder.join(name);
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(types)
}

/// Luau's strict type check of each script there is, against its own API; every problem is listed before refusing.
fn check_scripts(luau: &LuauChecker, types: &Types, keyboard: Option<&str>, options: Option<&str>, host: Option<&str>) -> Result<()> {
    let mut problems = Vec::new();
    if let Some(keyboard) = keyboard {
        let definitions =
            [LuauFile { name: "canvas.d.luau", text: definitions::CANVAS }, LuauFile { name: "inputs.d.luau", text: &types.inputs }];
        problems.extend(luau.check(&definitions, LuauFile { name: KEYBOARD_SCRIPT, text: keyboard })?);
    }
    if let Some(options) = options {
        problems.extend(luau.check_options(options, &types.options)?);
    }
    if let Some(host) = host {
        problems.extend(luau.check_host(host, &types.options)?);
    }
    if !problems.is_empty() {
        bail!("the canvas's scripts don't type-check:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// Fails with every problem `widget_problems` finds.
pub fn check_widget(luau: &LuauChecker, folder: &Path) -> Result<()> {
    let problems = widget_problems(luau, folder)?;
    if !problems.is_empty() {
        bail!("the widget's scripts don't type-check:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

/// A widget's `options.luau` and `host.luau`: run the first pass, write their types into
/// `.glassdeck/`, and type-check both, as "name:line:column: message". `keyboard.luau` keeps the
/// widget API's own rules.
pub fn widget_problems(luau: &LuauChecker, folder: &Path) -> Result<Vec<String>> {
    let options = read_optional(folder, OPTIONS_SCRIPT)?;
    let host = read_optional(folder, HOST_SCRIPT)?;
    let panel = match &options {
        Some(options) => OptionsScript::new(options).first_pass()?,
        None => Panel { items: Vec::new() },
    };
    let generated = panel.definitions();
    let types = folder.join(TYPES_FOLDER);
    fs::create_dir_all(&types).with_context(|| format!("creating {}", types.display()))?;
    for (name, text) in [
        ("options-api.d.luau", definitions::OPTIONS_API),
        ("options.d.luau", generated.as_str()),
        ("common.d.luau", definitions::COMMON),
        ("host.d.luau", definitions::HOST),
    ] {
        let path = types.join(name);
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    let mut problems = Vec::new();
    if let Some(options) = &options {
        problems.extend(luau.check_options(options, &generated)?);
    }
    if let Some(host) = &host {
        problems.extend(luau.check_host(host, &generated)?);
    }
    Ok(problems)
}
