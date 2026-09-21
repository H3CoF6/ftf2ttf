use ftf2ttf::convert_ftf;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
struct FontResource {
    id: u32,
    success: bool,
    url: String,
    size: u64,
}

fn download_file(url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        return Err(format!("Failed to download: {}", response.status()).into());
    }
    let bytes = response.bytes()?;
    Ok(bytes.to_vec())
}

fn extract_zip(zip_data: &[u8], extract_dir: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let cursor = std::io::Cursor::new(zip_data);
    let mut archive = zip::ZipArchive::new(cursor)?;

    fs::create_dir_all(extract_dir)?;

    let mut ttf_files = Vec::new();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let file_name = file.name().to_string();

        if file_name.to_lowercase().ends_with(".ttf") {
            let out_path = extract_dir.join(&file_name);
            let mut out_file = fs::File::create(&out_path)?;
            std::io::copy(&mut file, &mut out_file)?;
            ttf_files.push(out_path.to_string_lossy().to_string());
        }
    }

    Ok(ttf_files)
}

#[test]
fn test_download_extract_convert_pipeline() {
    let resources_path = Path::new("resources/font_urls.json");
    let resources_content = fs::read_to_string(resources_path)
        .expect("Failed to read resources/font_urls.json");

    let resources: Vec<FontResource> = serde_json::from_str(&resources_content)
        .expect("Failed to parse JSON");

    let test_ids = vec![20125, 22003, 20563];
    let tmp_dir = Path::new("tmp/download_test");

    if tmp_dir.exists() {
        fs::remove_dir_all(tmp_dir).ok();
    }
    fs::create_dir_all(tmp_dir).expect("Failed to create tmp directory");

    for id in test_ids {
        let resource = resources.iter().find(|r| r.id == id);
        if resource.is_none() {
            println!("⚠ Font ID {} not found in resources", id);
            continue;
        }

        let resource = resource.unwrap();
        println!("\n=== Processing Font ID: {} ===", id);
        println!("URL: {}", resource.url);

        println!("Downloading...");
        let zip_data = download_file(&resource.url)
            .unwrap_or_else(|_| panic!("Failed to download font {}", id));
        println!("✓ Downloaded {} bytes", zip_data.len());

        let extract_dir = tmp_dir.join(format!("{}_extracted", id));
        println!("Extracting to {:?}...", extract_dir);
        let ttf_files = extract_zip(&zip_data, &extract_dir)
            .unwrap_or_else(|_| panic!("Failed to extract font {}", id));
        println!("✓ Extracted {} TTF file(s)", ttf_files.len());

        for ttf_path in ttf_files {
            println!("Converting: {}", ttf_path);
            let input_data = fs::read(&ttf_path)
                .unwrap_or_else(|_| panic!("Failed to read {}", ttf_path));

            let output_path = format!("{}_converted.ttf", ttf_path.trim_end_matches(".ttf"));

            match convert_ftf(&input_data) {
                Ok(output_data) => {
                    fs::write(&output_path, output_data.clone())
                        .unwrap_or_else(|_| panic!("Failed to write {}", output_path));

                    println!("✓ Converted successfully");
                    println!("  Input size:  {} bytes", input_data.len());
                    println!("  Output size: {} bytes", output_data.len());
                    println!("  Output file: {}", output_path);

                    assert!(!output_data.is_empty(), "Output should not be empty");
                    assert!(
                        output_data.starts_with(&[0x00, 0x01, 0x00, 0x00]),
                        "Output should have valid TTF header"
                    );
                }
                Err(e) => {
                    println!("✗ Conversion failed: {}", e);
                    panic!("Conversion failed for {}: {}", ttf_path, e);
                }
            }
        }

        println!("✓ Font ID {} completed successfully\n", id);
    }

    println!("\n=== All tests passed! ===");
}
