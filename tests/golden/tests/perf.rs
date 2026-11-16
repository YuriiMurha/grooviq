//! The render benchmark times what an export renders: the same bits as `render_offline_with`,
//! at every block size it measures.

use grooviq_engine::{EngineConfig, render_offline_with};
use grooviq_golden::SAMPLE_RATE;
use grooviq_golden::perf::{hash, measure, perf_scenes};

/// About a second, a whole number of blocks at every size.
const FRAMES: usize = 192 * 256;

#[test]
fn the_timed_render_matches_the_offline_render_at_every_block_size() {
    let config = EngineConfig {
        sample_rate: SAMPLE_RATE as f32,
    };
    for scene in perf_scenes() {
        let reference =
            render_offline_with(config, scene.state.clone(), &scene.events(), FRAMES, 64);
        assert!(
            reference.iter().any(|x| x.abs() > 0.1),
            "{} renders silence",
            scene.name
        );
        for block in [64, 128, 256] {
            let m = measure(&scene, block, FRAMES, 1).unwrap();
            assert_eq!(
                m.hash,
                hash(&reference),
                "{} at {block} frames differs from the offline render",
                scene.name
            );
            assert_eq!(m.block_ns.len(), FRAMES / block);
            assert_eq!(m.speed.len(), 1);
        }
    }
}
