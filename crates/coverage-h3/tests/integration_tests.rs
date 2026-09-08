use coverage_h3::{export_rhgt_to_h3_jsonl, extract_h3_records, H3ExportConfig, H3Record};
use coverage_storage::{Metadata, NO_DATA};

fn create_test_metadata(width: u32, height: u32, resolution_m: f64, range_m: f64) -> Metadata {
    let half_w = (width as f64 * resolution_m) / 2.0;
    let half_h = (height as f64 * resolution_m) / 2.0;
    Metadata {
        radar_id: "test-radar-1".to_string(),
        los_algorithm_version: 3,
        radar_config_hash: "hash123".to_string(),
        terrain_hash: "terrain123".to_string(),
        calculated_at: "2026-09-08T08:00:00Z".to_string(),
        crs: "+proj=aeqd +lat_0=43.5 +lon_0=6.5 +R=6371000 +units=m".to_string(),
        origin: [-half_w, -half_h],
        resolution_m,
        extent: [-half_w, -half_h, half_w, half_h],
        width,
        height,
        range_m,
        effective_earth_k: 1.3333333333333333,
        nodata: NO_DATA,
    }
}

#[test]
fn test_nodata_raster_prunes_completely() {
    let meta = create_test_metadata(50, 50, 100.0, 2500.0);
    let heights = vec![NO_DATA; 50 * 50];
    let config = H3ExportConfig::default();

    let (records, stats) = extract_h3_records(&meta, &heights, &config).unwrap();
    assert_eq!(records.len(), 0);
    assert_eq!(stats.exported_cells, 0);
    assert!(stats.pruned_coarse_cells > 0);
}

#[test]
fn test_single_visible_pixel_exports_cell() {
    let width = 50;
    let height = 50;
    let meta = create_test_metadata(width, height, 100.0, 2500.0);
    let mut heights = vec![NO_DATA; (width * height) as usize];

    // Put a visible pixel at the radar center (row = 25, col = 25)
    let center_idx = (25 * width + 25) as usize;
    heights[center_idx] = 175; // 175 meters AGL

    let config = H3ExportConfig {
        target_resolution: 7,
        start_resolution: 5,
        altitude_bucket_m: Some(100),
        include_boundary: true,
    };

    let (records, stats) = extract_h3_records(&meta, &heights, &config).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(stats.exported_cells, 1);
    assert_eq!(records[0].min_floor_m, 175);
    assert_eq!(records[0].altitude_bucket_m, Some(100)); // 175 / 100 * 100 = 100

    let boundary = records[0].boundary.as_ref().unwrap();
    assert_eq!(boundary.len(), 7); // Closed polygon: 6 vertices + first point repeated
    assert_eq!(boundary[0], boundary[6]); // Closed loop
    for pt in boundary {
        let lon = pt[0];
        let lat = pt[1];
        assert!((lat - 43.5).abs() < 0.1);
        assert!((lon - 6.5).abs() < 0.1);
    }
}

#[test]
fn test_lowest_altitude_wins() {
    let width = 60;
    let height = 60;
    let meta = create_test_metadata(width, height, 50.0, 1500.0);
    let mut heights = vec![NO_DATA; (width * height) as usize];

    // Put multiple pixels with different heights near center
    heights[(30 * width + 30) as usize] = 350;
    heights[(30 * width + 31) as usize] = 120; // lowest
    heights[(31 * width + 30) as usize] = 200;

    let config = H3ExportConfig {
        target_resolution: 7,
        start_resolution: 6,
        altitude_bucket_m: Some(50),
        include_boundary: false,
    };

    let (records, stats) = extract_h3_records(&meta, &heights, &config).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].min_floor_m, 120);
    assert_eq!(records[0].altitude_bucket_m, Some(100)); // 120 / 50 * 50 = 100
    assert!(records[0].boundary.is_none());
    assert_eq!(stats.min_altitude_m, Some(120));
    assert_eq!(stats.max_altitude_m, Some(120));
}

#[test]
fn test_jsonl_streaming_and_determinism() {
    let width = 40;
    let height = 40;
    let meta = create_test_metadata(width, height, 100.0, 2000.0);
    let mut heights = vec![NO_DATA; (width * height) as usize];

    heights[(20 * width + 20) as usize] = 80;
    heights[(25 * width + 25) as usize] = 140;

    let config = H3ExportConfig::default();

    let mut buf1 = Vec::new();
    let stats1 = export_rhgt_to_h3_jsonl(&meta, &heights, &config, &mut buf1).unwrap();

    let mut buf2 = Vec::new();
    let stats2 = export_rhgt_to_h3_jsonl(&meta, &heights, &config, &mut buf2).unwrap();

    assert_eq!(stats1, stats2);
    assert_eq!(buf1, buf2); // Deterministic output

    let text = String::from_utf8(buf1).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(!lines.is_empty());

    for line in lines {
        let parsed: H3Record = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.res, 7);
        assert!(parsed.min_floor_m > 0);
    }
}

#[test]
fn test_configurable_resolution_8_and_9() {
    let width = 50;
    let height = 50;
    let meta = create_test_metadata(width, height, 50.0, 1500.0);
    let mut heights = vec![NO_DATA; (width * height) as usize];
    heights[(25 * width + 25) as usize] = 95;

    // Test Res 8
    let config_res8 = H3ExportConfig {
        target_resolution: 8,
        start_resolution: 6,
        altitude_bucket_m: None,
        include_boundary: true,
    };
    let (records_8, _) = extract_h3_records(&meta, &heights, &config_res8).unwrap();
    assert_eq!(records_8.len(), 1);
    assert_eq!(records_8[0].res, 8);

    // Test Res 9
    let config_res9 = H3ExportConfig {
        target_resolution: 9,
        start_resolution: 7,
        altitude_bucket_m: None,
        include_boundary: true,
    };
    let (records_9, _) = extract_h3_records(&meta, &heights, &config_res9).unwrap();
    assert_eq!(records_9.len(), 1);
    assert_eq!(records_9[0].res, 9);
}

#[test]
fn test_real_rhgt_dataset_if_present() {
    let p1 = std::path::Path::new(
        "data/results/11111111-1111-4111-8111-111111111111-3f8f0b5dcdc2df86.rhgt",
    );
    let p2 = std::path::Path::new(
        "../../data/results/11111111-1111-4111-8111-111111111111-3f8f0b5dcdc2df86.rhgt",
    );
    let path = if p1.exists() {
        p1
    } else if p2.exists() {
        p2
    } else {
        println!("No test .rhgt found, skipping real file test");
        return;
    };

    let config = H3ExportConfig {
        target_resolution: 7,
        start_resolution: 5,
        altitude_bucket_m: Some(100),
        include_boundary: true,
    };

    let mut output = Vec::new();
    let stats = coverage_h3::export_rhgt_file_to_h3_jsonl(path, &config, &mut output).unwrap();

    println!("Real dataset stats: {:?}", stats);
    assert!(stats.exported_cells > 0);
    assert!(stats.pruned_coarse_cells > 0);
    assert!(output.len() > 0);
}
