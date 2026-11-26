//! The canvas renderer's core, shared by glassdeck-gpu on the keyboard and the receiver's preview on
//! the PC: the shader program, the script that drives it, and the built-in gradient.

#[cfg(windows)]
pub mod offscreen;
pub mod program;
pub mod script;

/// `canvases/gradient`, packed; `the_built_in_gradient_matches_its_sources` checks it's current.
pub const BUILT_IN: &[u8] = include_bytes!("../builtin/gradient.deckbg");

#[cfg(test)]
mod tests {
    use glassdeck_package::{Origin, CanvasFiles, CanvasScripts, canvas_sources};

    #[test]
    fn the_built_in_gradient_matches_its_sources() {
        let read = |path: &str| std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        let built_in = CanvasFiles::from_bytes(super::BUILT_IN).unwrap();
        let Origin::Packed { sources, .. } = built_in.origin else { panic!("the built-in gradient wasn't made by glassdeck pack") };
        let current = canvas_sources(
            &read("../glassdeck-slang/slang/glassdeck.slang"),
            &read("../../canvases/gradient/canvas.slang"),
            CanvasScripts { keyboard: Some(&read("../../canvases/gradient/keyboard.luau")), options: None, host: None },
        );
        assert_eq!(
            sources, current,
            "canvases/gradient or the glassdeck API changed; repack: glassdeck pack canvases/gradient --out crates/glassdeck-gpu/builtin/gradient.deckbg"
        );
    }
}
