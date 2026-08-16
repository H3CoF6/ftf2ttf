use std::fs;
use std::path::Path;

fn is_ftf_font(raw: &[u8]) -> bool {
    if raw.len() < 12 {
        return false;
    }
    let num_tables = u16::from_be_bytes([raw[4], raw[5]]) as usize;
    if raw.len() < 12 + num_tables * 16 {
        return false;
    }

    for i in 0..num_tables {
        let off = 12 + i * 16;
        if off + 4 > raw.len() {
            break;
        }
        let tag = &raw[off..off + 4];
        if tag == b"FTFH" || tag == b"FTFG" {
            return true;
        }
    }
    false
}

#[test]
fn test_convert_20352() {
    let input = Path::new("resources/20352/20352.ttf");
    assert!(input.exists(), "Test file resources/20352/20352.ttf not found");

    let raw = fs::read(input).expect("Failed to read 20352.ttf");

    if is_ftf_font(&raw) {
        let result = ftf2ttf::convert_ftf(&raw);
        assert!(result.is_ok(), "Failed to convert 20352.ttf: {:?}", result.err());
        let converted = result.unwrap();
        assert!(!converted.is_empty(), "Converted file is empty");
        assert!(converted.len() > 12, "Converted file too short");
    } else {
        println!("20352.ttf is already a normal TTF, skipping conversion test");
    }
}

#[test]
fn test_convert_20402() {
    let input = Path::new("resources/20402/20402.ttf");
    assert!(input.exists(), "Test file resources/20402/20402.ttf not found");

    let raw = fs::read(input).expect("Failed to read 20402.ttf");

    if is_ftf_font(&raw) {
        let result = ftf2ttf::convert_ftf(&raw);
        assert!(result.is_ok(), "Failed to convert 20402.ttf: {:?}", result.err());
        let converted = result.unwrap();
        assert!(!converted.is_empty(), "Converted file is empty");
        assert!(converted.len() > 12, "Converted file too short");
    } else {
        println!("20402.ttf is already a normal TTF, skipping conversion test");
    }
}

#[test]
fn test_convert_20563() {
    let input = Path::new("resources/20563/20563.ttf");
    assert!(input.exists(), "Test file resources/20563/20563.ttf not found");

    let raw = fs::read(input).expect("Failed to read 20563.ttf");
    assert!(is_ftf_font(&raw), "20563.ttf should be an FTF font");

    let result = ftf2ttf::convert_ftf(&raw);
    assert!(result.is_ok(), "Failed to convert 20563.ttf: {:?}", result.err());
    let converted = result.unwrap();
    assert!(!converted.is_empty(), "Converted file is empty");
    assert!(converted.len() > 12, "Converted file too short");
}

#[test]
fn test_convert_22004() {
    let input = Path::new("resources/22004/22004.ttf");
    assert!(input.exists(), "Test file resources/22004/22004.ttf not found");

    let raw = fs::read(input).expect("Failed to read 22004.ttf");
    assert!(is_ftf_font(&raw), "22004.ttf should be an FTF font");

    let result = ftf2ttf::convert_ftf(&raw);
    assert!(result.is_ok(), "Failed to convert 22004.ttf: {:?}", result.err());
    let converted = result.unwrap();
    assert!(!converted.is_empty(), "Converted file is empty");
    assert!(converted.len() > 12, "Converted file too short");
}

#[test]
fn test_convert_20183() {
    let input = Path::new("resources/20183/20183.ttf");
    assert!(input.exists(), "Test file resources/20183/20183.ttf not found");

    let raw = fs::read(input).expect("Failed to read 20183.ttf");

    if is_ftf_font(&raw) {
        let result = ftf2ttf::convert_ftf(&raw);
        assert!(result.is_ok(), "Failed to convert 20183.ttf: {:?}", result.err());
        let converted = result.unwrap();
        assert!(!converted.is_empty(), "Converted file is empty");
        assert!(converted.len() > 12, "Converted file too short");
    } else {
        println!("20183.ttf is already a normal TTF, skipping conversion test");
    }
}

#[test]
fn test_convert_20268() {
    let input = Path::new("resources/20268/20268.ttf");
    assert!(input.exists(), "Test file resources/20268/20268.ttf not found");

    let raw = fs::read(input).expect("Failed to read 20268.ttf");

    if is_ftf_font(&raw) {
        let result = ftf2ttf::convert_ftf(&raw);
        assert!(result.is_ok(), "Failed to convert 20268.ttf: {:?}", result.err());
        let converted = result.unwrap();
        assert!(!converted.is_empty(), "Converted file is empty");
        assert!(converted.len() > 12, "Converted file too short");
    } else {
        println!("20268.ttf is already a normal TTF, skipping conversion test");
    }
}

#[test]
fn test_convert_22003() {
    let input = Path::new("resources/22003/22003.ttf");
    assert!(input.exists(), "Test file resources/22003/22003.ttf not found");

    let raw = fs::read(input).expect("Failed to read 22003.ttf");

    if is_ftf_font(&raw) {
        let result = ftf2ttf::convert_ftf(&raw);
        assert!(result.is_ok(), "Failed to convert 22003.ttf: {:?}", result.err());
        let converted = result.unwrap();
        assert!(!converted.is_empty(), "Converted file is empty");
        assert!(converted.len() > 12, "Converted file too short");
    } else {
        println!("22003.ttf is already a normal TTF, skipping conversion test");
    }
}

#[test]
fn test_convert_22001() {
    let input = Path::new("resources/22001/22001.ttf");
    assert!(input.exists(), "Test file resources/22001/22001.ttf not found");

    let raw = fs::read(input).expect("Failed to read 22001.ttf");
    assert!(is_ftf_font(&raw), "22001.ttf should be an FTF font");

    let result = ftf2ttf::convert_ftf(&raw);
    assert!(result.is_ok(), "Failed to convert 22001.ttf: {:?}", result.err());
    let converted = result.unwrap();
    assert!(!converted.is_empty(), "Converted file is empty");
    assert!(converted.len() > 12, "Converted file too short");
}
