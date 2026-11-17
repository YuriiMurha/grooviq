use grooviq_engine::{SampleBuf, waveform::*};
use std::sync::Arc;
#[test]
fn exact_ranges_signs_and_partial_bins() {
    let l: Vec<_> = (0..1031).map(|i| i as f32 / 1031.0 - 0.5).collect();
    let sample = Arc::new(SampleBuf {
        rate: 48000.0,
        r: l.iter().map(|v| -v).collect(),
        l,
    });
    let pyramid = PeakPyramid::new(sample.clone()).unwrap();
    for (start, end, bins) in [(0, 1031, 7), (1, 1030, 33), (1029, 1031, 2), (0, 1, 1)] {
        let peaks = pyramid.query(start, end, bins).unwrap();
        for (i, p) in peaks.iter().enumerate() {
            let a = start + (end - start) * i / bins;
            let b = start + (end - start) * (i + 1) / bins;
            assert_eq!(*p, Peak::from_frames(&sample.l[a..b], &sample.r[a..b]));
        }
    }
    assert!(pyramid.query(0, 1032, 1).is_err());
    assert!(pyramid.query(0, 1, 16385).is_err());
    assert!(pyramid.query(1, 1, 1).is_err());
}
#[test]
fn silence_impulse_tiny_and_invalid_buffers() {
    for length in [1, 63, 64, 65, 4097] {
        let mut l = vec![0.0; length];
        l[length - 1] = 1.0;
        let tree = PeakPyramid::new(Arc::new(SampleBuf {
            rate: 8000.0,
            r: l.iter().map(|v| -v).collect(),
            l,
        }))
        .unwrap();
        assert_eq!(tree.query(length - 1, length, 1).unwrap()[0].r_min, -1.0);
        assert!(tree.peak_bytes() <= (length.div_ceil(64) * 2 + 16) * 16);
    }
    for sample in [
        SampleBuf {
            rate: 48000.0,
            l: vec![],
            r: vec![],
        },
        SampleBuf {
            rate: 1.0,
            l: vec![0.0],
            r: vec![0.0],
        },
        SampleBuf {
            rate: 48000.0,
            l: vec![f32::NAN],
            r: vec![0.0],
        },
    ] {
        assert!(PeakPyramid::new(Arc::new(sample)).is_err());
    }
}
#[test]
fn rust_binary_fixture() {
    let tree = PeakPyramid::new(Arc::new(SampleBuf {
        rate: 8000.0,
        l: vec![-1.0, 0.5, 0.25],
        r: vec![1.0, -0.5, -0.25],
    }))
    .unwrap();
    let bytes = tree.encode(9, 10, 11, 0, 3, 2).unwrap();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/waveform-v1.bin");
    if std::env::var_os("GROOVIQ_WRITE_WAVEFORM_FIXTURE").is_some() {
        std::fs::write(&path, &bytes).unwrap();
    }
    assert_eq!(bytes, std::fs::read(path).unwrap());
}

#[test]
fn request_bin_cap_is_independent_of_source_length() {
    let tree = PeakPyramid::new(Arc::new(SampleBuf {
        rate: 48000.0,
        l: vec![0.0; 20000],
        r: vec![0.0; 20000],
    }))
    .unwrap();
    assert!(tree.query(0, 20000, 16385).is_err());
    assert_eq!(tree.query(0, 20000, 16384).unwrap().len(), 16384);
}

#[test]
fn nonmonotonic_interior_levels_match_direct_scan() {
    let left: Vec<_> = (0..20003)
        .map(|i| ((i * 7919 % 1009) as f32 - 504.0) / 505.0)
        .collect();
    let right: Vec<_> = (0..20003)
        .map(|i| ((i * 3571 % 1013) as f32 - 506.0) / 507.0)
        .collect();
    let sample = Arc::new(SampleBuf {
        rate: 48000.0,
        l: left,
        r: right,
    });
    let tree = PeakPyramid::new(sample.clone()).unwrap();
    for (start, end, bins) in [
        (0, 20003, 1),
        (3, 19999, 7),
        (65, 18001, 127),
        (0, 20003, 16384),
    ] {
        for (index, peak) in tree.query(start, end, bins).unwrap().iter().enumerate() {
            let a = start + (end - start) * index / bins;
            let b = start + (end - start) * (index + 1) / bins;
            assert_eq!(*peak, Peak::from_frames(&sample.l[a..b], &sample.r[a..b]));
        }
    }
}
