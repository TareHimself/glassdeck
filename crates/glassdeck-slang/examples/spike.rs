use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glassdeck_slang::{Slang, Source, Target, essl};

fn main() -> Result<()> {
    let native = PathBuf::from(std::env::var("GLASSDECK_SLANG_NATIVE").context("set GLASSDECK_SLANG_NATIVE to the wrapper's bin folder")?);
    let out = PathBuf::from(std::env::args().nth(1).context("give an output folder")?);
    std::fs::create_dir_all(&out)?;
    let slang = Slang::load(&native)?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |p: &str| std::fs::read_to_string(root.join(p)).with_context(|| p.to_owned());
    let (api, scene, glue) =
        (read("slang/glassdeck.slang")?, read("tests/scenes/gradient.slang")?, read("tests/scenes/gradient_glue.slang")?);
    let module = |name, path, text| Source { name, path, text };
    let all =
        [module("glassdeck", "glassdeck.slang", &api), module("gradient", "gradient.slang", &scene), module("glue", "glue.slang", &glue)];
    let glsl = String::from_utf8(slang.compile(&all, "gd_fragment", Target::Glsl, "glsl_450")?)?;
    std::fs::write(out.join("fragment.glsl"), &glsl)?;
    std::fs::write(out.join("fragment.essl"), essl::fragment_to_essl(&glsl)?)?;
    println!("wrote {}", out.join("fragment.essl").display());
    Ok(())
}
