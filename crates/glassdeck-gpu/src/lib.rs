//! The canvas renderer's core, shared by glassdeck-gpu on the keyboard and the receiver's preview on
//! the PC: the shader program, the script that drives it, and the built-in gradient.

#[cfg(windows)]
pub mod offscreen;
pub mod program;
pub mod script;

/// `canvases/gradient`, packed; `the_built_in_gradient_matches_its_sources` checks it's current.
pub const BUILT_IN: &[u8] = include_bytes!("../builtin/gradient.deck");

/// The built-in gradient, ready to run.
pub fn built_in() -> anyhow::Result<glassdeck_package::CanvasFiles> {
    use anyhow::Context;
    glassdeck_package::CanvasFiles::read(&glassdeck_package::Package::built_in(BUILT_IN)?).context("opening the built-in gradient")
}

#[cfg(test)]
mod tests {
    use glassdeck_package::{CanvasScripts, Origin, canvas_sources};

    #[test]
    fn the_built_in_gradient_matches_its_sources() {
        let read = |path: &str| std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        let built_in = super::built_in().unwrap();
        let Origin::Packed { sources, .. } = built_in.origin else { panic!("the built-in gradient wasn't made by glassdeck pack") };
        let current = canvas_sources(
            &read("../../canvases/gradient/deck.json"),
            &read("../glassdeck-slang/slang/glassdeck.slang"),
            &read("../../canvases/gradient/canvas.slang"),
            CanvasScripts { keyboard: Some(&read("../../canvases/gradient/keyboard.luau")), options: None, host: None },
        );
        assert_eq!(
            sources, current,
            "canvases/gradient or the glassdeck API changed; repack: glassdeck pack canvases/gradient --out crates/glassdeck-gpu/builtin/gradient.deck"
        );
    }
}
