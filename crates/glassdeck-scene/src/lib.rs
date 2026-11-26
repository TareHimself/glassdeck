//! The scene renderer's core, shared by glassdeck-scene on the keyboard and the receiver's preview on
//! the PC: the shader program, the script that drives it, and the built-in gradient.

#[cfg(windows)]
pub mod offscreen;
pub mod program;
pub mod script;

/// `scenes/gradient`, packed; `the_built_in_gradient_matches_its_sources` checks it's current.
pub const BUILT_IN: &[u8] = include_bytes!("../builtin/gradient.deckbg");

#[cfg(test)]
mod tests {
    use glassdeck_package::{Origin, SceneFiles, scene_sources};

    #[test]
    fn the_built_in_gradient_matches_its_sources() {
        let read = |path: &str| std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        let built_in = SceneFiles::from_bytes(super::BUILT_IN).unwrap();
        let Origin::Packed { sources, .. } = built_in.origin else { panic!("the built-in gradient wasn't made by glassdeck pack") };
        let current = scene_sources(
            &read("../glassdeck-slang/slang/glassdeck.slang"),
            &read("../../scenes/gradient/scene.slang"),
            Some(&read("../../scenes/gradient/keyboard.luau")),
        );
        assert_eq!(
            sources, current,
            "scenes/gradient or the glassdeck API changed; repack: glassdeck pack scenes/gradient --out crates/glassdeck-scene/builtin/gradient.deckbg"
        );
    }
}
