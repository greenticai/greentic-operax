use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("failed to encode JSON for {}: {err}", path.display()))?;
    bytes.push(b'\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    fs::write(path, bytes).map_err(|err| format!("failed to write {}: {err}", path.display()))
}

pub fn extract_zip_to_dir(pack_path: &Path, target: &Path) -> Result<(), String> {
    if target.exists() {
        fs::remove_dir_all(target)
            .map_err(|err| format!("failed to clean {}: {err}", target.display()))?;
    }
    fs::create_dir_all(target)
        .map_err(|err| format!("failed to create {}: {err}", target.display()))?;
    let file = fs::File::open(pack_path)
        .map_err(|err| format!("failed to open {}: {err}", pack_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|err| format!("failed to read zip {}: {err}", pack_path.display()))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("failed to read zip entry {index}: {err}"))?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name() else {
            return Err(format!("unsafe zip entry path `{}`", entry.name()));
        };
        let out_path = target.join(name);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
        }
        let mut out = fs::File::create(&out_path)
            .map_err(|err| format!("failed to create {}: {err}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|err| format!("failed to extract {}: {err}", out_path.display()))?;
    }
    Ok(())
}

pub fn pack_dir(source: &Path, pack_path: &Path) -> Result<(), String> {
    let tmp = pack_path.with_extension("gtpack.tmp");
    let file = fs::File::create(&tmp)
        .map_err(|err| format!("failed to create {}: {err}", tmp.display()))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for path in sorted_files(source)? {
        let rel = path
            .strip_prefix(source)
            .map_err(|err| format!("failed to compute relative zip path: {err}"))?
            .to_string_lossy()
            .replace('\\', "/");
        writer
            .start_file(rel, options)
            .map_err(|err| format!("failed to start zip entry: {err}"))?;
        let bytes =
            fs::read(&path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
        writer
            .write_all(&bytes)
            .map_err(|err| format!("failed to write zip entry: {err}"))?;
    }
    writer
        .finish()
        .map_err(|err| format!("failed to finish {}: {err}", tmp.display()))?;
    fs::rename(&tmp, pack_path).map_err(|err| {
        format!(
            "failed to replace {} with {}: {err}",
            pack_path.display(),
            tmp.display()
        )
    })
}

pub fn sorted_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_files(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(root).map_err(|err| format!("failed to read {}: {err}", root.display()))?
    {
        let entry = entry.map_err(|err| format!("failed to read directory entry: {err}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn write_json_creates_file_with_trailing_newline() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.json");
        write_json(&path, &json!({"key": "value"})).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.ends_with('\n'));
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn write_json_creates_parent_directories() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("a/b/c/test.json");
        write_json(&path, &json!(42)).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn write_json_overwrites_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("test.json");
        write_json(&path, &json!(1)).unwrap();
        write_json(&path, &json!(2)).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed, json!(2));
    }

    #[test]
    fn sorted_files_returns_sorted_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("c.txt"), b"c").unwrap();
        fs::write(temp.path().join("a.txt"), b"a").unwrap();
        fs::write(temp.path().join("b.txt"), b"b").unwrap();
        let files = sorted_files(temp.path()).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn sorted_files_includes_nested_directories() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("sub")).unwrap();
        fs::write(temp.path().join("top.txt"), b"top").unwrap();
        fs::write(temp.path().join("sub/nested.txt"), b"nested").unwrap();
        let files = sorted_files(temp.path()).unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn sorted_files_empty_directory() {
        let temp = tempfile::tempdir().unwrap();
        let files = sorted_files(temp.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn sorted_files_nonexistent_directory() {
        let err = sorted_files(Path::new("/nonexistent/path")).unwrap_err();
        assert!(err.contains("failed to read"));
    }

    #[test]
    fn pack_and_extract_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("sub")).unwrap();
        fs::write(source.join("hello.txt"), b"hello world").unwrap();
        fs::write(source.join("sub/nested.txt"), b"nested content").unwrap();

        let pack_path = temp.path().join("test.gtpack");
        pack_dir(&source, &pack_path).unwrap();
        assert!(pack_path.exists());

        let extracted = temp.path().join("extracted");
        extract_zip_to_dir(&pack_path, &extracted).unwrap();
        assert_eq!(
            fs::read_to_string(extracted.join("hello.txt")).unwrap(),
            "hello world"
        );
        assert_eq!(
            fs::read_to_string(extracted.join("sub/nested.txt")).unwrap(),
            "nested content"
        );
    }

    #[test]
    fn extract_zip_cleans_existing_target() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file.txt"), b"content").unwrap();

        let pack_path = temp.path().join("test.gtpack");
        pack_dir(&source, &pack_path).unwrap();

        let target = temp.path().join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("stale.txt"), b"stale").unwrap();

        extract_zip_to_dir(&pack_path, &target).unwrap();
        assert!(!target.join("stale.txt").exists());
        assert!(target.join("file.txt").exists());
    }

    #[test]
    fn extract_zip_nonexistent_pack() {
        let temp = tempfile::tempdir().unwrap();
        let err = extract_zip_to_dir(
            &temp.path().join("missing.gtpack"),
            &temp.path().join("out"),
        )
        .unwrap_err();
        assert!(err.contains("failed to open"));
    }

    #[test]
    fn pack_dir_replaces_existing_pack() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("v1.txt"), b"v1").unwrap();

        let pack_path = temp.path().join("test.gtpack");
        pack_dir(&source, &pack_path).unwrap();

        fs::write(source.join("v2.txt"), b"v2").unwrap();
        pack_dir(&source, &pack_path).unwrap();

        let extracted = temp.path().join("extracted");
        extract_zip_to_dir(&pack_path, &extracted).unwrap();
        assert!(extracted.join("v2.txt").exists());
    }
}
