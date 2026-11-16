//! The render benchmark times what an export renders: the same bits as `render_offline_with`,
//! at every block size it measures.

use grooviq_engine::{EngineConfig, render_offline_with};
use grooviq_golden::SAMPLE_RATE;
use grooviq_golden::perf::{hash, measure, perf_scenes};

/// About a second, a whole number of blocks at every size.
const FRAMES: usize = 192 * 256;
/// Timed repetitions: more than one, so each repetition's numbers are checked separately.
const REPS: usize = 2;

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
            let m = measure(&scene, block, FRAMES, REPS).unwrap();
            assert_eq!(
                m.hash,
                hash(&reference),
                "{} at {block} frames differs from the offline render",
                scene.name
            );
            assert_eq!(m.block_ns.len(), REPS * FRAMES / block);
            assert_eq!(m.speed.len(), REPS);
            // Each repetition's speed is its audio time over the sum of its own block times.
            let audio = FRAMES as f64 / f64::from(SAMPLE_RATE);
            for (rep, times) in m.block_ns.chunks(FRAMES / block).enumerate() {
                let total: u64 = times.iter().sum();
                assert!(total > 0, "{} rep {rep}: no time recorded", scene.name);
                let expected = audio / (total as f64 * 1e-9);
                assert!(
                    (m.speed[rep] - expected).abs() <= 1e-9 * expected,
                    "{} rep {rep}: speed {} but its blocks give {expected}",
                    scene.name,
                    m.speed[rep]
                );
            }
        }
    }
}
